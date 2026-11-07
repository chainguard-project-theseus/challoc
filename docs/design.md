# challoc design

This document describes challoc as it is built. Where it says "must", an
implementation that does otherwise is wrong. Numbers appear only where
they justify a current default; how numbers are produced is in
[benchmarking.md](benchmarking.md), and the operator's view of every key
is [tuning.md](tuning.md). Section numbers are stable: code comments cite
them as "design N".

## 1. Scope and constraints

- Linux only, 64-bit only: x86_64 and aarch64. The page size is read from
  `AT_PAGESZ` at init and may be 4, 16 or 64 KiB.
- Supported environment: Chainguard OS and Wolfi containers with glibc
  2.44 or newer, on host kernels 5.14 (RHEL 9) and newer. Kernel 5.10 is
  kept as a CI tripwire, not a promise: it must keep working, with
  reduced features, so a regression shows up before it reaches the floor.
  (The `tripwire` check is off until its Debian 11 bench image is
  rebuilt, #463; `.chainguard/ci/floor.yaml` says why and how to restore it.
  Until then nothing checks 5.10.)
  Other libcs, distributions and operating systems are not supported.
- Built as a `no_std` Rust crate with `panic = "abort"`, producing a
  `cdylib` and a `staticlib`. The crate never allocates through anything
  but its own machinery, never calls `fopen`, `opendir`, `dlopen` or
  `pthread_setspecific` from an allocation path, and `free` preserves
  `errno`. These are glibc's documented rules for a replacement malloc.
- Stable Rust. Thread-local storage uses a hand-written initial-exec
  accessor (section 6.1), never `thread_local!`.
- Consumers are C programs (Valkey, Ruby and other Wolfi packages). There
  is no Rust `GlobalAlloc` facade.
- Every kernel and CPU feature is probed at run time and has a fallback
  (section 17). The same binary runs on every supported kernel and CPU:
  a feature is found by trying a syscall on a scratch page, reading
  `AT_HWCAP` or a sysfs file, never by `-C target-cpu` or a header
  constant. A feature that is on by default when present may change only
  cost, memory and detection, never observable behaviour (allocation
  results, abort conditions), and every probe has a word in the feature
  report (`challoc_features()`, section 16).
- Any heap inconsistency aborts. The allocator never tries to continue.

## 2. Address space layout

At init challoc reserves address space with `mmap(PROT_NONE, MAP_PRIVATE |
MAP_ANONYMOUS | MAP_NORESERVE)` and never unmaps it. All memory handed to
callers lives inside these reservations. Committing memory means
`mprotect` to `PROT_READ | PROT_WRITE` on a range of an already reserved
region; the kernel provides zero pages on first touch.

```
+------------------+  metadata region     (reserved, committed on demand)
| heaps, stacks    |  object side, bump-allocated from the base
| ...              |
| slab tables      |  per class: descriptors, slot bitmap, owner array
+------------------+
| gap              |  (sealed with mseal where the kernel has it)
+------------------+  class region 0      (randomised offset inside its gap)
| slab | slab | .. |
+------------------+
| gap              |  (sealed)
+------------------+  class region 1
| ...              |
+------------------+  huge region
| run | guard | .. |
+------------------+
```

There are three mappings: the class superblock (every class region and
its gap), the huge region and the metadata region (`layout/regions.rs`,
`Regions::reserve`). The parts that no code ever commits, purges or
reprotects (the gaps and the alignment slack, `Regions::never_committed`)
are sealed with `mseal` at init when the kernel has it (`Regions::seal`),
so nothing can land a foreign mapping between two class regions or unmap
part of a reservation. The regions themselves cannot be sealed: sealing
forbids the `mprotect` that commits a slab and the `madvise` that purges
it. Each sealed range splits its VMA (about 110 more lines in
`/proc/self/maps` at quantum 16). Without `mseal`, or under a seccomp
profile that filters it (Docker's default), nothing is sealed and nothing
else changes. The region table (`Regions`, in the library's data segment)
is not sealed either: it shares pages with run-time counters, and `mseal`
does not stop writes.

Inside the metadata region (`meta.rs`) objects (heaps, cache slot
stacks, huge descriptors) grow from a bump pointer at the base, committed
in 64 KiB chunks, and the never-freed tables (a class's descriptor array,
slot bitmap and owner array, an arena's set tables, the huge free-page
bitmap, the background thread's stack) are reserved downward from the top
and committed page by page as slab numbers grow. Each committed table
prefix is its own VMA, so to keep the mapping count (and the cost of
`fork`) down a table of at most `meta::EAGER_MAX` (8 MiB) is carved from
the object side instead, writable as a whole (`Meta::reserve_table`;
its owner's first commit makes the object side writable through every
table carved so far, in one `mprotect`, so the dozens of tables init
carves cost one call; later commits are accounting only). The small classes' large
descriptor arrays and bitmaps stay on the table side: writable whole they
would put gigabytes of untouched address space into core dumps and under
`RLIMIT_DATA`. Eager carving is off under `vm.overcommit_memory=2` (which
ignores `MAP_NORESERVE` and would charge every forked child), under a
finite `RLIMIT_DATA` (which since Linux 4.7 charges every private
writable mapping: the eager tables are about 85 MiB writable from the
first `malloc`, against under 2 MiB of `VmData` with them on the table
side; a working set still needs about twice glibc's `VmData` under a
data limit, every arena's commits counting, and past it `malloc` returns
NULL and recovers) and
under any `thp` key but `never` (`meta::eager_policy`). The reservations
themselves are `PROT_NONE` and `MAP_NORESERVE`, which neither
`RLIMIT_DATA` nor the overcommit heuristic charges; only commits count.

Sizes are runtime parameters chosen by probing how much virtual address
space the process can reserve, so the same binary works on 47-bit x86_64,
48-bit aarch64 and 39-bit aarch64 kernels (`layout/regions.rs`):

| Parameter          | Preferred     | Minimum      |
|--------------------|---------------|--------------|
| Class region size  | 2^38 (256 GiB)| 2^32 (4 GiB) |
| Gap per class      | 3 x region (stride 4 x) | 1 x region (stride 2 x) |
| Huge region        | 2^40 (1 TiB)  | 2^34         |
| Metadata region    | 2^36          | 2^30         |

The minimum layout is the least address space init needs: at quantum 16
`NCLASSES` = 52, so 52 x 2^33 for the superblock, 2^34 for the huge
region and 2^30 for the metadata region, each with its slack (4 MiB of
alignment, plus 1 GiB of slide for the two side regions): about 435 GiB,
which a 39-bit (512 GiB) kernel or an `RLIMIT_AS` of 436 GiB or more
holds (quantum 8, with 56 classes, needs 468 GiB or more).
Below it init aborts with `cannot reserve ...` (section 14.1).

Region bases are randomised with `getrandom` and the three mappings are
reserved in random order. Each shrinks until the kernel grants it, so
while one is reserved a `PROT_NONE` placeholder holds the minimum layouts
of the ones drawn after it (`layout::regions::hold`): under an
address-space limit the first one drawn can never take what the others
need at their smallest, and any limit that covers the minimum layout
on top of the program's own mappings (436 GiB at quantum 16) reserves. The huge and metadata regions slide by 1 to 255
steps of 4 MiB (just under 8 bits) above their mapping's 4 MiB-aligned
start. Inside the superblock the class regions sit at
a fixed power-of-two stride in class order, each at a random offset
inside its gap (1 to gap / 4 MiB - 2 steps of 4 MiB, about 17.6 bits at
the preferred layout)
plus a fixed *colour*: `21 * c` units of 64 KiB modulo 4 MiB, rounded
down to the class's slab and the page (`layout::regions::colour`; none
under `thp:always` chunks, which are measured from the region base).
Every class allocates its lowest slabs first, so without the colour the
first pages of every class sit at the same address modulo 4 MiB and
crowd the same few sets of a set-associative second-level TLB (256 sets
of 4 KiB pages span 1 MiB on Neoverse N1/N3/V2): on `larson1` at 1000
rounds per thread on Neoverse N3 that was up to 20 live pages in one set,
and page walks per operation fell from 0.105 to 0.064 with the colour
(jemalloc 0.039). The colours of any 16 consecutive classes are
distinct modulo 1 MiB, 320 KiB apart. The reservation
prefers a stride of 4 x the region, shrinks the region towards 2^32
before giving that up, and only then drops to a stride of 2 x
(`layout::reserve_superblock`). Classifying a pointer is therefore
`c = (p - superblock) >> lg_stride` and a range check against class
`c`'s region; else the huge region, where the descriptor is looked up by
page; else invalid, abort.

No draw is ever zero or the largest step, so every region keeps at least
4 MiB of never-committed (and, with `mseal`, sealed) reservation below
and above it, independent of the random values and of where `mmap` put
the mapping: class region 0 above the superblock's start, each class
region below the next, the last class below the superblock's end, and
the huge and metadata regions inside their mappings
(`layout::regions::slab_multiple`). An overflow or underflow off any
region's edge faults there instead of reaching a neighbouring mapping;
in particular the first huge run, which has a guard page only after it,
has this slack before it. Init checks the property (`Regions::slack_ok`)
and aborts if it ever fails.

## 3. Size classes

Two quantum modes, selected at build time by Cargo feature (exactly one):

- `quantum16` (default): 16-byte minimum alignment as required by
  `max_align_t`. Classes: 16, 32, 48, 64, 80, 96, 112, 128, then four
  classes per doubling (160, 192, 224, 256, 320, ...) up to the largest
  medium class.
- `quantum8`: matches jemalloc built with `--with-lg-quantum=3`, which is
  what Redis and Valkey vendor and what Valkey's defragmenter asserts via
  the `arenas.quantum` control. Classes: 8, 16, 24, 32, 40, 48, 56, 64, 80,
  96, 112, 128, then four per doubling. Allocations of 8 bytes are
  8-aligned. `aligned_alloc` and friends always honour their requested
  alignment.

Tiers (`layout/mod.rs`; `SMALL_MAX` 1024, `MEDIUM_MAX` 256 KiB):

| Tier   | Size range              | Slab size          | Slots per slab |
|--------|-------------------------|--------------------|----------------|
| Small  | quantum .. 448          | 64 KiB             | 146 .. 8192    |
| Small  | 512 .. 1024             | 4 .. 16 KiB, or the page when larger (up to 64 KiB) | 4 .. 25 (64 .. 128 at 64 KiB) |
| Medium | 1280 .. 256 KiB         | max(64 KiB, 16 * size) | >= 16      |
| Huge   | > 256 KiB               | page-granular runs | 1              |

The 512..1024-byte classes use the smallest power-of-two slab from 4 KiB
up whose internal waste is at most a thirty-second of the slab
(`layout::LG_MIN_SLAB`, `SLAB_WASTE_DIV`): 4 KiB for 512 and 1024 bytes,
8 KiB for 896, 16 KiB for 640 and 768. These are the classes where a few
long-lived survivors (Valkey's values after a delete phase) would pin a
whole 64 KiB slab each. Every other class keeps 64 KiB: a 16 KiB slab
for every class doubled the per-slab refill and flush work and halved
`xmalloc-test`, and a class with 64 or more slots per slab rarely empties
a slab by chance anyway.

On a kernel whose page is larger than a class's slab (16 and 64 KiB pages
on aarch64) the class takes the page as its slab size, up to 64 KiB,
decided once at init (`layout::adopt_page_geometry`): at 64 KiB pages the
five classes get 64 KiB slabs of 128, 102, 85, 73 and 64 slots
(jemalloc's `regs` at `LG_PAGE 16`). Every `Empty` slab is then one purge
unit; sub-page slabs would be purged in page groups, only when every slab
in the group is idle, and a workload that frees at random leaves `Empty`
slabs in groups that never are. Page groups remain only for a faked page
in the tests. The geometry is the class table itself, rewritten in place
at init (`lg_slab`, `nslots`, `slab_bits` and `slab_mask` of the raised
classes), so the
`free` and `sdallocx` fast paths still load the class line from a
link-time constant address and their machine code is unchanged on 4 KiB
hosts; the table lives in writable memory as a result, like the
`Regions`. The compaction unit (`LG_COMPACT_UNIT`) and the guard interval
unit (`LG_GUARD_UNIT`) stay 64 KiB whatever a class's slab size.

Slab number and slot index are:

```
slab   = (p - base_c) >> lg_slab_c
offset = p & slab_mask_c                   // slab_size_c - 1
slot   = (offset * inv_odd_c).rotate_right(tz_c)   // 32-bit
valid  = slot < nslots_c
```

The slot-start test is one multiply, one rotate and one compare
(Granlund and Montgomery's divisibility test,
`Class::slot_of_offset` in `layout/`). With `size_c = d << tz_c`, `d` odd,
`inv_odd_c` is `d`'s inverse modulo `2^32`: when `size_c` divides `offset`,
the product is `(offset / size_c) << tz_c` and the rotate gives the slot.
Any other offset either has a set bit below `tz_c`, which the rotate moves
into the top bits, or is a multiple of `2^tz_c` but not of `d`, whose
product lands above `(2^(32 - tz_c) - 1) / d`; both are at least `nslots_c`
because `nslots_c * size_c` is at most the slab, `2^22`. The test accepts
exactly the slot starts, with the same slot as a division would give,
which `slot_of_offset_is_exactly_the_reciprocal_check` checks for every
byte offset of every class's slab at every supported page size. Each table
entry is 32 bytes (20 of fields), so the entry's address is one shifted
add.

The size class of a request uses the same lookup shape as jemalloc and
mimalloc: a small table for requests up to 1024 bytes and a
leading-zero-count computation above that. `nallocx` returns the class
size. The table holds the class times the 64-byte stride of the
per-class lines (cache headers, class lines), and `malloc` and the
sized free reach a size above it through a tail call, so on their fast
paths the entry addresses the lines as it is: no copy, no shift and no
register spent on the scaled class.

## 4. Slab descriptors and bitmaps

Every slab has a descriptor in the metadata region (`slab.rs`).
Descriptors for class `c` form a dense array indexed by slab number at a
per-class stride of 16 to 64 bytes (`max(16, 64 / slabs_per_line)`, so
that the descriptors of the slabs sharing one bitmap line fill whole
descriptor lines), committed on demand as the region grows. The
descriptor is a fixed 16-byte header:

```
struct SlabDesc {
    state:     AtomicU8,     // Fresh, Active, Empty, Purged
    shadow:    AtomicU8,     // state + 1, written with every change; check_zero tests it
    arena:     AtomicU16,
    live:      AtomicU32,    // slots currently marked allocated
    dirty_at:  AtomicU64,    // monotonic ns when it became Empty, 0 otherwise
}
```

The allocation bits are not in the descriptor. Each class has one
contiguous *slot bitmap* in the metadata region, committed on demand like
the descriptor array, in which slab `s` owns bits `s * slab_bits .. (s +
1) * slab_bits`. `slab_bits` is `nslots` padded to a power of two below
512 or to a multiple of 512 from there (`Class::slab_bits`), so a slab's
bits are either whole words or one range inside a word, never straddling
a word, and a 64-byte line holds a whole number of slabs. The padding
bits between `nslots` and `slab_bits` are kept permanently set. At one bit
per slot the bitmap of a real heap is a few hundred KB, which stays in
the cache hierarchy where a descriptor line per slab would not: it is
what makes the exact `free` check (section 6.3) affordable.

Slabs are handed to arenas in *groups* of `slabs_per_line` consecutive
slabs (the slabs whose bits fill one bitmap line), lowest group first to
whichever arena needs a slab next, and each arena commits its group's
slabs as it uses them. So a bitmap line and a descriptor line are written
only by one arena's refills and flushes, and the `free` check never reads
a line another arena's threads write. A dense *owner array* of one `u16`
per group records each group's arena; `free` reads it to route a slot
back to its arena. The owner word and the descriptors' arena field are
written together only when a group is handed out
(`SlabTable::take_group`), which aborts if the two disagree; a group never
changes arena after that, and a slab number carried from a lock-free step
into a lock hold (`reopen`, `on_live_zero`) is checked against the owner
word under the lock. A group spans at most 2 MiB (`LG_GROUP_SPAN`):
classes with larger slabs pad each slab's bits to a whole line instead.

Bitmap semantics: a set bit means the slot is not available to a slab
refill; it is either live in the program or sitting in some thread's
cache. A clear bit means the slot belongs to the slab and may be claimed.
Operations on the bitmap are always atomic. Slabs have no owning thread.

- **Claim** (cache refill): find the lowest word with any clear bit of
  the slab, compute `want = !word & slab_mask` masked to at most `n` bits,
  `fetch_or(want)`; the bits that were actually clear in the returned
  value are the claimed slots. Retry on the next word if fewer than
  requested. Add the count to `live`.
- **Release** (cache flush): group pointers by slab, `fetch_and(!mask)`
  per word, subtract from `live`. A bit that is already clear aborts (an
  invalid or double free caught at the flush; a set bit is not proof
  that the cached copy was valid: a refill may have claimed the slot
  since the free, 6.3). If `live` reaches zero,
  transition the slab to `Empty`, stamp `dirty_at`, and put it on the
  arena's empty set for the class.
- **Check** (free): load the bitmap word, verify the bit is set, else
  abort (`invalid or double free of <p> (class, slab, slot): slot is not
  allocated`; a slot start in a guard slab or in a slab never allocated
  from gets its own message). No write.

Slot randomisation (`slot_randomize`, off by default): a cache refill or
run rebuild (`ClassState::claim_random`) plans the same slabs as the
ordered claim, but the bit scan starts at a word drawn from the heap's
xorshift (seeded from `getrandom` on first use) and the claimed batch is
shuffled. The set of slots is unchanged, so a `Fresh` or `Purged` slab's
batch is still known zero; direct claims stay address-ordered. It is off
by default because it fights the address-ordered policy of section 5 and
the compaction of 6.4, and costs RSS for that reason.

## 5. Per-class state and slab selection

Each class has one process-wide `SlabTable` (`class/table.rs`): the
class's descriptor array, slot bitmap and owner array and, behind its own
lock, the commit watermark and the cursor that hands slab groups to
arenas (section 4). Each `(arena, class)` pair has a `ClassState`
(`class/mod.rs`) over that table:

```
struct SlabTable {                    // one per class, shared by every arena
    class, lg_group, base, nslabs, desc_base,
    inner: Lock<{ descs, bits, owners, // metadata reservations, committed on demand
                  committed_slabs, next_group, guards }>,
}

struct ClassState {                   // one per (arena, class)
    class:  u16,
    arena:  u16,
    table:  *const SlabTable,
    gate:   purge::Decayer,           // oldest Empty dirty_at (read lock-free by decay), next walk deadline
    inner:  Padded<Lock<{             // the class lock, taken on slab transitions only
        partial, empty, purged,       // Sets of slab numbers
        stats,                        // committed, guards, empty, purged, nonfull
        group_next, group_end,        // this arena's current slab group
    }>>,
    claimed, released, refilled,      // slot counters: curregs = claimed - released
    compacted, compact_on, compact_off, gate_refill,   // compaction diagnostics (6.4, 16)
}
```

The lock (`lock.rs`, every `Lock` and `RawLock` in the allocator) is a
futex mutex in the shape of Drepper's "mutex, take 3": one CAS to acquire
uncontended, one swap to release, `FUTEX_WAKE` only when the word says a
waiter may sleep. A contended locker spins 100 rounds (`SPINS`, one
`core::hint::spin_loop` each) before `FUTEX_WAIT`: slow-path critical
sections are a few dozen instructions.

The slab sets (`class/sets.rs`) are two-level bitmaps over slab numbers
(`Set`) that give "lowest slab in the set" in two `tzcnt` instructions.
An arena's three sets per class (`partial`: at least one clear bit;
`empty`: `live == 0`, not yet purged; `purged`) are views of one
interleaved table per (arena, slab size) (`SetTable`), so the words a
small heap touches share one page per (arena, slab size) instead of one
or two pages per `(arena, class)`.

Refill takes the class lock, reads the lowest partial slab, releases the
lock, then claims from that slab's bitmap without the lock. Contention on
the same slab between two threads is resolved by the atomic claim. The
exception is a slab that was `Fresh` or `Purged` at selection: its slots
(the lowest `n` bits; `live` is zero, so nothing contends) are claimed in
the same lock hold, before the slab enters the partial set
(`claim_prefix`), and the refill reports them zero. Decided after the
lock is dropped, a bit could be one that another thread claimed from the
now-partial slab, handed to the program and got back dirty in between,
and the zero report (which `check_zero` reads) would be false. No
zeroing entry point trusts the report for a slab slot (section 6.3).
Slots of an `Active` slab are never reported zero. When a slab's bitmap becomes completely set,
the thread that observed it clears the slab's partial bit under the
lock; when a release clears bits in a slab that was full, the releasing
thread sets the partial bit under the lock. The lock is taken once per
refill, that is once per `cap / 2` allocations per thread in steady state
(512 for the small classes at the default capacity). It is the only lock
on the allocation path.

When there is no partial slab, take the lowest `Empty` slab if one
exists, else the lowest `Purged` slab, else commit the next slab of the
arena's current group (`class/fresh.rs`), else take the next group from
the class table. Committing means `mprotect` to read-write; fresh slabs
are zero from the kernel. An arena commits its group a run at a time
(`SlabTable::commit_run`): the slabs from its cursor up to the next guard
slab, at most 512 KiB of them (`RUN_BYTES`, the default guard interval
of 64 KiB slabs), with one `mprotect`, and the guard slab, when the
cursor reaches it, with the run after it. Without the cap the class of
xmalloc's 32 KiB batches committed runs of several MiB and the row lost
6..9% on Axion. The guard slab that ends a run stays uncommitted until
then, so a write running off the end of a run faults there, and a
process that never fills a run pays for no guard. Inside a run the slabs
ahead of the cursor are read-write before they are handed out, so an
overflow off the arena's newest slab faults only at the run's end, not
at the next slab as with one commit per slab (`docs/hardening.md`).
Each `mprotect` that grows a mapping takes the process's `mmap_lock` for
writing and stalls every thread's page faults behind it: one per slab
cost rptest's 16 threads more than 3000 commits per run and a quarter
of their timed work off the CPU. Where a run starts follows from the
slab number (`SlabTable::starts_run`: a group's first slab, a guard
slab, every 512 KiB), so the arena keeps no run cursor, and the syscalls
run without the class table's commit lock, which only counts guards.
Committing whole groups at hand-out, or the class region as one prefix
ahead of the arenas, cut the commits further but was not kept: it
installs guards in groups a short-lived process never fills
(pg-connect20 +6%, worse, on the x86_64 dev VM) and merges the region
into one VMA every faulting thread shares. Page groups larger than a
slab (only the tests' faked pages) and `thp:always` chunks are committed
slab by slab (`SlabTable::commit_slab`, which commits a slab's page
group, for slabs smaller than a page). The `thp` key adds two branches to the commit path, both off under the
default `never` (section 17.1): `take_group` issues `MADV_HUGEPAGE` over
the group's range when the class qualifies under `auto`, and
`commit_slab` under `always` commits and advises the PMD-sized chunk
around the slab.

Address-ordered first fit is deliberate. It is what jemalloc does, and it
is what keeps long-lived heaps dense and makes defragmentation converge.

Guard slabs: with `guards` on, every `guard_slab_interval`-th 64 KiB of
slabs is a *guard slab* that is never handed out
(`SlabTable::is_guard_slab`: a shift, an add and a mask; the interval is
a power of two so the sized fast path's test needs no division). A guard
slab gets `MADV_GUARD_INSTALL` on its first page while still
`PROT_NONE`, then is committed read-write with the slabs after it (the
marker survives the `mprotect`), so the VMA merges and a write running
off the previous slab faults; if the install fails (an `mlock`ed range)
the slab stays `PROT_NONE` and only the slabs after it are committed
(14.1).
Without `MADV_GUARD_INSTALL` (before 6.13) `Shared::init` forces the
interval to 0, because a `PROT_NONE` slab every eighth slab would split
each class region into thousands of VMAs; `guards:false` sets it to 0
too. A guard slab whose marker is refused (`mlock`) stays a `PROT_NONE`
slab for good, as slabs are never decommitted: at most two VMAs per
`guard_slab_interval` x 64 KiB of the class region ever committed,
bounded by the region's peak footprint, not by live blocks (14.1). A guard slab inside a
`thp:always` chunk is left unused but unguarded (section 17.1).

## 6. Thread heaps and caches

### 6.1 Thread-local access

One pointer-sized TLS slot, declared in `.tbss` via `global_asm!`, read
and written by two small `asm!` sequences (`sys/tls.rs`):

- x86_64: `mov rax, [rip + challoc_tls@GOTTPOFF]; mov rax, fs:[rax]`
- aarch64: `adrp x0, :gottprel:challoc_tls; ldr x0, [x0, :gottprel_lo12:challoc_tls]; mrs x1, tpidr_el0; ldr x0, [x1, x0]`

This is the initial-exec model. glibc reserves static TLS for objects
present at program start, which covers both linking against
`libchalloc.so` and `LD_PRELOAD`. It does not cover `dlopen` of challoc
after start; that is not a supported configuration.

Signed heap pointer (`sys/pac.rs`). With `pac_pointers:true` on an arm64
host that exposes pointer authentication (`HWCAP_PACA` and `HWCAP_PACG`;
`Shared::pac()`), the slot holds the heap pointer signed with the data
key A and the thread pointer as modifier (`pacda heap, tpidr_el0`), so a
heap pointer copied into another thread's slot fails; the accessor loads
`tpidr_el0` anyway (`tls::get_tp`). The sentinels (`TLS_CREATING = 1`,
`TLS_EXITED = 2`, null) are never signed and are compared first. A
corrupted slot faults at the `autda` (FEAT_FPAC) or on the first header
load. On x86_64 the code compiles to the plain accessor.

### 6.2 Heap structure

```
struct Heap {                                // repr(C); caches at offset 128
    shared:      *const Shared,
    tid:         u32,
    arena:       u16,                        // home arena: plain malloc allocates here
    slow_ticks:  u32,                        // counts slow-path entries for periodic work
    hint_ticks:  u32,                        // counts get_defrag_hint calls (13.5)
    last_gc:     u64,                        // monotonic ns of the last cache GC
    uncached:    ThreadStats,                // bytes that never went through a cache (huge, direct)
    others:      *mut ArenaCaches,           // caches of other arenas, and the FOREIGN node (6.3)
    snapshot:    ThreadStats,                // thread.allocated/deallocated as of the last read
    tcache_enabled: bool,                    // thread.tcache.enabled
    bound:       bool,                       // counted in the home arena's load
    rng:         u64,                        // slot_randomize's xorshift, seeded lazily
    caches:      [CacheHeader; NCLASSES],    // home arena, inline, one 64-byte line per class
}

struct CacheHeader {                         // one per (arena, class), 64 bytes, line aligned
    len, cap:        u32,                    // cap is 0 until first use
    low_water:       u32,                    // for GC
    zeroed:          u32,                    // slots[..zeroed] read as zero
    slots:           *mut *mut u8,           // this class's stack, its own metadata object
    allocs:          u64,                    // pops served since creation
    compact_mask:    u32,                    // probe mask, COMPACT_MASK, or off (6.4)
    run_end:         u32,                    // slots[..run_end] is the run of low addresses (6.4)
    moved:           u64,                    // refilled - flushed (for thread.deallocated)
    probe_refilled:  u32,                    // the class's refill counter at the previous probe
    arena:           u16,
    mode:            u16,                    // off, probe, compact (6.4)
    unsorted_flushes: u8,                    // drives the flush's periodic kept-half sort (6.4)
}
```

The headers are dense inside the heap: `malloc` reaches `len` at `heap +
128 + 64 * c` with no pointer load, and a thread's headers share one
page. Each class's stack is its own metadata object, allocated on the
class's first slow path (`Heap::ensure_cache`), so a thread pays only for
the classes it touches; one contiguous slot block per heap cost a freeing
thread three to four times the L1D fills. The per-thread byte counters
are not kept per operation: `allocs` is bumped on the line a pop writes
anyway and the free count follows from the stack arithmetic. Heaps and
stacks come from the metadata region's object side (section 2); heaps are
created on a thread's first allocation, and an explicit cache
(`tcache.create`) is a `Heap` owned by no thread.

Signed entries. With `Shared::pac()` set, every pointer in `slots[..len]`
is stored signed with the data key A and the stack's base address as the
modifier (`pacda p, slots`): pushes, refills and rebuilds sign; pops and
the flush authenticate (`autda`). The modifier is the stack, not the
entry's position, so entries can move within a stack, and it is loaded
by every pop and push anyway. The flush orders entries by their address
bits alone (`!pac::mask()`) and authenticates only the half it releases,
so an entry costs one `pacda` in and one `autda` out. A corrupted entry
never comes back from `malloc` usable: with FEAT_FPAC the pop's `autda`
faults (`SIGILL`); on the original PAuth the pointer comes out
non-canonical and faults on first use (`SIGSEGV`). The PAC is 7 bits on
Linux's default 48-bit VA, so a forged entry passes one time in 128 and
every failure is a crash
(`heap::linux_tests::corrupted_cache_entry_or_heap_pointer_faults`).

Default cache capacity (`heap::MAX_CAP`, `MIN_CAP`; `config::ladder`):
1024 slots for classes up to 128 bytes, then halving every doubling of
class size, minimum 8; classes above `tcache_max` (32 KiB, jemalloc's
default) are not cached and go straight to the slab bitmaps.
`tcache_nslots_small_max` and `lg_tcache_nslots_mul` from `malloc_conf`
override the ladder, because Redis sets them. `Config::cap_for(home,
arena, class)` is the whole rule (zero means "never cached here"). A
cache for another arena (`others`) holds nothing above the bin classes
(4 KiB, `BIN_MAX`): its residue is multiplied by the arena count, and at
16 threads and 16 arenas eight cached 4..32 KiB slots per (thread, arena,
class) were 190 MiB of `mstress`'s RSS. The capacity is larger than
jemalloc's 200 because a flush or refill writes the bitmap words and
descriptor lines that other threads' `free` checks read; halving the
flush rate halves those misses. Cache geometry: a smaller, L2-derived
ladder for small-cache CPUs did not move the rows it was meant to help;
`tcache_nslots_small_max:512` is the knob for a smaller cache residue.

### 6.3 Fast paths

The fast paths are in `heap/paths.rs` and `heap/cache.rs`; the exported
entry points in `api/` are built from them.

`malloc(n)`:
1. `c = class_of(n)`; if huge, slow path.
2. `heap = tls()`; if null or a sentinel, slow path (create heap, may
   init the allocator).
3. `cache = heap.caches[c]` (the home arena's header line); if `len ==
   0`, slow path (refill).
4. `p = slots[--len]` (`autda p, slots` when `PUSH_PAC`, 6.2); return `p`.

`free(p)`:
1. Classify `p` (section 2). Huge: slow path. Invalid: abort. Null is
   in no class region and leaves for the slow path, which returns for it
   (no test of its own on the fast path, as in jemalloc's `free`).
2. Compute slab and slot; verify the offset is a slot start (section 3:
   multiply, rotate, compare against `nslots`); load the class bitmap
   word; verify the bit is set. Otherwise abort. The slab is compared
   with the class's hand-out watermark before the bitmap load, and that
   compare is the region bound too (`ClassLine::set_slab`): an
   address past the region, or below its base where the offset wraps,
   has a slab number of at least `nslabs`, which no watermark reaches.
   Every per-class value this step reads (region base, slot geometry,
   bitmap base, a copy of the watermark stored after the table's own) sits
   in one 64-byte line per class, `Shared::lines`, whose stride is the
   cache headers', so one scaled class index addresses both.
   (Dropping the watermark instead, by reserving the bitmaps `PROT_READ`
   so a never-handed slab reads a clear bit, was measured and rejected:
   `mlockall` populates the 8 GiB of read-only tails with zero-page
   entries, 16 MiB of locked page tables per process, and
   `MADV_HUGEPAGE` does not help, since a huge zero page mapping still
   deposits its page table.)
3. The opt-in hardening: one byte (`Shared::free_flags`, `push_flags`
   plus the `FREE_ROUTE` bit of step 4, on a line step 2 already
   loaded), zero by default on every host with one arena (`pac_pointers`
   defaults to false, so a CPU with pointer authentication is no
   exception), tested once; `FREE_ROUTE` alone is the second compare.
   `PUSH_STATE` (`double_free_scan`): the push declines, and every cache
   is in `MODE_STATE`, whose scheduled-pop mask is 0, so the fast pop
   declines with the test it already makes (no flag in `malloc`); every
   free and every pop takes its slow path, where the slot's state byte
   (`heap/slotstate.rs`: one byte per slot next to the bitmap, `FREE`,
   `CACHED` or `LIVE`) is checked and moved (a free with one
   compare-and-exchange): a free of a slot that is not `LIVE` aborts at
   the call on every route (the cached slow path, which the plain,
   foreign and sized frees reach, and the direct free), a refill's claim
   takes `FREE` to `CACHED`, a pop `CACHED` to `LIVE`, a flush's release
   `CACHED` to `FREE`. The bytes of a group are committed when the group
   is handed to an arena, before the watermark a free compares first.
   `PUSH_ZERO` (`zero_on_free`): `memset` the slot's class-size bytes (a
   huge free clears the run's usable bytes). `PUSH_PAC`
   (`pac_pointers:true` on an arm64 host with PAC): sign the entry
   (6.2); arm64 compares the byte against `PUSH_PAC` alone first, and
   off arm64 that compare is compiled out, so the dispatch is the one
   test against zero. `double_free_scan` costs 1.8..2.9x throughput on
   the quick rows (`alloc-test1` 46..68%; cb run
   `20261102t0229z-caad429-quick-m5fdy`), every operation being a slow
   path; `zero_on_free`'s `memset` per free is far over a 3% budget on
   Valkey's 1 KiB values.
4. `heap = tls()`; with several arenas (`FREE_ROUTE`, set when a second
   arena is built) load `owner[slab / group]` and,
   if it is not the heap's home arena, take the cold path (`free_other`,
   which pushes onto the heap's one *foreign* cache for the class,
   `Heap::others` keyed `FOREIGN`, creating it on first use); otherwise
   `cache = heap.caches[c]`; if `len == cap`, slow path (flush half).
   `slots[len++] = p`.

The foreign cache (`heap/others.rs`) is flush-only: nothing pops from
it, its capacity is half the ladder's, it is released whole when full,
and its flush routes each run of slabs to the slab's arena
(`release_routed`). One per thread bounds the dead slots a process like
Ruby (whose GC sweep frees every thread's garbage from one thread) holds
in caches to less than jemalloc's tcache. Pushing foreign slots onto the
home cache instead, jemalloc's shape, hands a thread another arena's
slots and cost `larson` up to 10% on x86_64. The per-arena `others` nodes
remain for explicit `MALLOCX_ARENA` allocations and `thread.arena`.

`sdallocx(p, n, flags)`, `free_sized(p, n)`, `free_aligned_sized(p, a, n)`
(the sized path):
1. On aarch64, if `p` is null return; on x86_64 null has no test of its
   own: it is below every class region, so its `rel` (below) wraps past
   the region, and the slow path returns for it. `c_hint =
   class_for(n, align)` (huge: slow path). `sdallocx` with `flags == 0` (what Valkey passes on every free)
   tests that first and takes `free_sized`'s path, `c_hint = class_of(n)`,
   without decoding the flags (alignment shift, tcache bits), as
   jemalloc's `sdallocx` does; any other flags take a call
   (`sdallocx_flags`), so the compiler cannot merge the two inlined sized
   paths behind a test of the hint, and a huge size leaves for the slow
   path before the heap lookup. The hint names the region: `rel = p - class_base[c_hint]`; if
   `rel` is past the region or not a slot start, slow path, as one
   64-bit test: `x = rel & (slab_mask | region_hi)` keeps the in-slab
   offset and any bit past the region, and `(x * inv64).rotate_right(tz)
   < nslots` holds exactly for a slot start inside the region (past it,
   `x >= 2^32 > nslots * size`). A guard slab goes to the slow path too
   (`((rel >> lg_guard) + 1) & guard_mask == 0`, the page-group test).
   With `exact_sized_free` every line's `guard_mask` is 0, so this test
   sends every sized free to the slow path, whose exact `free` reads the
   slot bit at the call; the instructions are the same either way.
   The region base, the slot geometry and the guard word (`guard_mask`,
   `tz`, `lg_guard`: one 8-byte load) are read from the class's line
   (`Shared::lines[c]`, the one plain `free` reads); the class table line
   is not touched. The sized path's cost on Zen 4 is its count of loads
   that wait on the size or the pointer, not its instruction count: a
   Valkey `SET` frees a value whose pointer and size come from a cache
   miss, and `valkey1` on Genoa moved 8% between 16 and 13 such loads.
2. `heap = tls()`; `cache = heap.caches[c]`; if `len == cap`, slow path.
   `slots[len++] = p`. Neither the bitmap word nor the owner word is
   loaded.
3. Slow path: a slab pointer whose class is not `c_hint`, or the start
   of an active huge run whose page count `n` does not round to, aborts
   (jemalloc's contract, `nallocx(n, flags) == sallocx(p)`); otherwise
   the exact `free` above.

Neither path performs an atomic read-modify-write or takes a lock in the
default configuration (under `double_free_scan` every free takes the slow
path, whose state check is one compare-and-exchange).
`free`'s one bitmap load (and owner load with several arenas) is the
price of invalid-free detection (exact for a slot no cache holds: a slot
a refill claimed into a cache has its bit set before `malloc` hands it
out, so a free of it passes; `double_free_scan`'s state byte catches
that) and also what makes cross-thread frees free of coordination;
neither touches the slab descriptor.

The sized path skips that load, and with it the check: an invalid or
double free through it is caught when a flush releases the slot while
it is still unclaimed (the release aborts on a clear bit, and on a guard
slab, a `Fresh` slab or a slab past the table's watermark by name,
`SlabTable::owner`), whenever a flush reaches it: a partial flush keeps
the lowest addresses (6.4), so that is not bounded by a number of frees.
Until then the slot can be handed out twice, as under jemalloc, and a
slot that was not allocated at the call (a slot start challoc has not
handed out) can be claimed by a refill before the flush, which then
releases it under its new owner: the flush tests the slot's state at the
flush, not at the call. Because the arena is not
read either, the home cache holds slots of any arena on this path, and
the flush releases each run of slabs through the arena the owner array
names (`release_owned`).
Plain `free` keeps step 2's load, so a double `free` of a slot that has
left the cache aborts at the call. `MALLOCX_ARENA(a)` with the default
cache pops whatever the cache holds, as jemalloc's does;
`MALLOCX_TCACHE_NONE` releases straight to the slab with the exact check.

`calloc` always clears a slab slot with `memset`. The heap still tracks
which cached slots challoc has not written since the kernel zeroed them,
as a per-cache zero prefix: `slots[..zeroed]` are known zero, set by a
refill whose batch came from a `Fresh` or `Purged` slab (while
`purge::purged_is_zero` holds, section 8), lowered
with `len` by a pop that takes a zero entry and dropped by a flush.
`zeroed <= low_water` holds throughout, so only a pop that lowers
`low_water` can take a zero entry: the zero test and the shrink sit in
that pop's cold branch, and the push, which stores at `len >= zeroed`,
never touches `zeroed`. No zeroing entry point (`calloc`,
`challoc_calloc_usable`, `MALLOCX_ZERO`) trusts the flag for a slab
slot: the slot is read-write memory of the committed run, which a linear
overflow from a live slot below or a write through a stale pointer into
a purged slab reaches without challoc seeing it (SECURITY.md, "Zeroing
allocations are zero"; `tests/c/zero_trust.c` checks every zeroing
entry point against overflows, underflows and stale writes into memory
not yet handed out). `calloc` zeroes the requested bytes, not the whole
usable size.

`malloc_usable_size(p)` (and `sallocx`, and the size read of `realloc`
and `rallocx`) classifies and tests the bit as `free` does, aborting on a
clear bit, and returns the class size (a huge run's usable size from the
huge table). `realloc` returns the same pointer if the new size maps to
the same class, otherwise allocates, copies `min(old, new)`, frees.
`xallocx` never resizes in place and reports `sallocx(p)`, so a caller
that needs more falls back to `rallocx`, as jemalloc's contract allows.

### 6.4 Slow paths

- **Refill** (`heap/flush.rs`): claim up to `cap / 2` slots from the
  current or next partial slab (section 5), lowest address on top; the
  batch is the cache's *run* (below) and `low_water` restarts at its
  length. Count slow paths in `slow_ticks` and run periodic work every 64
  of them (`PERIODIC_EVERY`, `purge::on_tick`): cache GC if the interval
  has elapsed, then decay if a class's deadline has passed (or the start
  of the background thread that will do it), and the rate-limited cgroup
  sample (section 11). Thread statistics are merged by `epoch`, not here.
- **Flush**: release the `cap / 2` highest-address entries grouped by
  slab and keep the lowest (the run is dropped and rebuilt on demand).
  Update `live`; slabs reaching zero become `Empty`. The selection is by
  address, not by age, so every flush sends holes to the high slabs while
  refills and rebuilds keep taking the lowest partial slab. The split: a
  slot array already in address order (`is_sorted`) is split directly;
  otherwise `select_nth_unstable` puts the released half on top and
  `group_by_slab` groups it with a counting pass on the slab number (a
  sort when it spans more than 256 slabs). The release accumulates each
  slab's bits in a scratch bitmap and clears them, one atomic per touched
  word. The kept half is sorted only on the first two consecutive
  unsorted flushes and every 32nd after (`unsorted_flushes`), so a cache
  pushed in order gets back onto the sorted path; a full sort at every
  flush cost several percent of tiny-object churn workloads. Double
  frees: a duplicate inside the batch hits a set scratch bit and aborts;
  a pair that straddles the split is exactly a released entry equal to
  the kept maximum, and one compare aborts it; a pair kept together stays
  cached and consistent until a flush releases it.
- **Scheduled pop** (the compacting cache, `heap/compact.rs`): every
  `2^COMPACT_SHIFT` pops of a class (`COMPACT_SHIFT = 3`, one pop in 8)
  are served from the *run* of low addresses `slots[..run_end]`
  (descending, lowest at `run_end - 1`) instead of the top: the run's
  lowest entry is returned and the top entry moved into its place. The
  run is a refill's batch or a *rebuild*'s block: `cap / 4` slots claimed
  from the lowest partial slab and swapped into the bottom of the stack,
  after flushing the cache down to `cap / 2` if it has no room; the
  rebuild also runs the periodic work, which under a balanced churn is
  the only slow path a class takes. `run_end <= low_water <= len` keeps
  the run intact: a pop that dips below it clips both, a push never
  writes below `len`.

  Why: a LIFO cache hands every allocation the slot just freed, so under
  a balanced alloc/free churn (Valkey's DEL:SET steady state) the cache
  never reaches either boundary, no refill or flush runs, and the slab
  layout is frozen at whatever occupancy the delete phase left. The
  scheduled pop makes one allocation in `2^k` land in the lowest slab
  with room, so the live set migrates downward at `1/2^k` of the
  allocation rate and the high slabs drain, empty and are purged.

  The schedule is gated per cache, because compacting costs slab traffic
  and cold slots (`larson` lost 15..22% when it was unconditional): in
  *probe* mode one pop in 4096 (`PROBE_MASK`) asks the class for its holes
  (`ClassState::holes`) and switches to *compact* mode only if they are
  worth at least four 64 KiB slabs (`COMPACT_MIN_SLABS`) and 1/32 of the
  class (`COMPACT_MIN_RATIO`), and refills claimed less than two slabs'
  worth since the last probe (`COMPACT_MAX_REFILL_SLABS`; refills already
  drain from the bottom; direct claims do not count). A rebuild drops
  back to probing once the class is dense, which then pays one counter
  test per `malloc`.
- **Defrag GC**: `get_defrag_hint` calls run the GC below at most once
  per `DEFRAG_GC_INTERVAL_NS` (100 ms), with the clock read once every
  `PERIODIC_EVERY` calls (13.5); such a GC restarts the GC clock.
- **Refresh GC**: an `epoch` write (13.4) runs the GC below first when the
  last one is `DEFRAG_GC_INTERVAL_NS` old (`Heap::gc_if_due`). After a
  mass free (Redis's `FLUSHDB`) the freed slots wait in the freeing
  thread's caches, each keeping its slab occupied and counted as
  fragmentation by the figures the caller is about to read; Redis and
  Valkey write `epoch` from their 100 ms cron on the main thread, so the
  slots that went unused are returned within a cron period or two
  instead of the 10 s period. It reaches the calling thread's caches
  only: a thread that frees and then stays idle (Redis's `bio` threads)
  keeps what it holds until its own next slow path.
- **Cache GC**: every 10 s (`GC_INTERVAL_NS`) flush three quarters of
  `low_water`, the entries that went unused since the last GC, then reset
  `low_water = len`. `cap` is fixed; it never shrinks. This is jemalloc's
  incremental tcache GC in spirit. At or above 90% of a cgroup limit the
  interval is 0 (section 11): the GC runs at every periodic tick.

### 6.5 Thread exit

A `pthread_key` created at init has a destructor that flushes all of the
thread's caches and frees the heap (`heap/thread.rs`).
`pthread_setspecific` is called once at heap creation, guarded against
recursion by the `TLS_CREATING` sentinel in the slot; an allocation in
that window, or after the destructor has set `TLS_EXITED` (another key's
destructor), is served straight from the slab bitmaps with no cache
(`alloc_direct`). glibc does not allocate for the first 32 keys, so the
recursion path is expected never to run, but it must be correct. Past
them a thread's first `pthread_setspecific` in each block of 32 keys
allocates a second-level block and fails if that allocation does; the new heap is then freed and the call is served
with no cache, like one whose heap could not be allocated, and the next
call tries again.

When the last thread heap homed on an arena is destroyed, the arena's
`Empty` slabs are purged at once (jemalloc's `tcache_destroy` rule),
except with the background thread running and at most once per decay
period per arena (`Arena::take_exit_purge`), so a pool that recycles
threads does not purge and refault the same slabs at every exit. The
count is checked after the exit flush: no purge when another heap was
homed there meanwhile.

Successor threads. A thread that hands its data to a thread it starts
and then exits (larson's workers) would otherwise put the successor on
another arena, since the arena is still held when the successor creates
its heap: every `free` of the inherited data took the `FOREIGN` cache
while the successor's allocations came from fresh refills of its own
arena, and successive threads alternated between two arenas. Two rules
keep a successor on the data's arena:

- The exiting heap is unbound before its exit flush, not after, so a
  successor that creates its heap while the flush runs is homed on the
  idle arena directly (`Arenas::assign`).
- A successor that was homed earlier moves its home onto the arena of
  the data it frees (`Heap::try_rehome`). It is looked at on the
  `FOREIGN` slow path only (the first cross-arena free of each class,
  whose `FOREIGN` cache does not exist yet, and the flush of a full
  one), so the cross-arena fast push carries no test for it, and moves
  while all of these hold: the heap is young, fewer than `REHOME_TICKS`
  (8) slow-path entries, so it has refilled at most a few caches and has
  little data of its own to strand; the last two frees looked at went
  to that arena (an inherited data set comes from one arena, the
  transfer slots of mstress's workers from all of them); the arena is
  automatic and no heap is homed there (one compare-and-swap of its
  bound count from 0 to 1, so two candidates never both take it); and
  the heap keeps no `others` node for it. The move flushes the home and
  `FOREIGN` caches through the ordinary flush, so nothing stays cached
  across it and a stale free of a slot cached before it aborts as
  before, and re-keys the home headers to the new arena. The free that
  triggers the move can itself be a double free of a slot on the
  `FOREIGN` cache (a full cache sends the second free to the slow path),
  and the move's flush releases the first copy; after it the exact bit
  test runs
  again on the slot before the home push, which aborts as `invalid or
  double free` instead of caching a slot a refill could hand out too. At
  most once per heap, and never under `double_free_scan`, whose state
  check must come first; a `thread.arena` write and an explicit cache never
  move. A young thread's `thread.arena` therefore reads back a different
  arena after a move it did not ask for.

## 7. Huge allocations

Requests above the largest medium class are served from the huge region
with page granularity (`huge/`).

- A page-granular `class::Set` tracks free pages; allocation is
  lowest-address first fit of `ceil(n / page) + GUARD_PAGES` contiguous
  pages, under the huge lock.
- An open-addressing hash table keyed by page number (`huge/table.rs`)
  holds `HugeDesc { start, pages, usable, arena, state, dirty_at, prev,
  next }`: a run is keyed by its start page, a free extent by its first
  and last page, so a free finds both neighbours and free extents are
  always coalesced. `free` of a huge-region pointer that is not a run
  start aborts. The table's load stays at or below one half, every
  allocation also reserves a key for each live run's free (14.1), and
  `insert` aborts rather than fill the last slot.
- Runs are followed by a guard page (with `guards` on, the default):
  `MADV_GUARD_INSTALL` when available, which splits no VMA and survives a
  later commit of the neighbouring run; else (kernels before 6.13) a
  `PROT_NONE` page, which costs no memory but splits the VMA: a live
  guarded run costs at most two more VMAs (the guard, and the split
  that separates the run's pages from the free memory after it) while
  it lives. A free makes the `PROT_NONE` guard read-write again, so the
  free extent merges back into its neighbours and the split ends with
  the run: below the mark every page is read-write except the guards of
  live runs, and the VMAs the huge tier holds are bounded by its live
  runs. The kernel merges two anonymous VMAs only when they share an
  `anon_vma`, which a VMA gets at its first write fault, so a run
  reaching past the mark is committed through its guard and its first
  page is populated for write (`MADV_POPULATE_WRITE`, or a zero written
  to its first byte where a filter refuses that) before the guard is
  split off (`split_guard`); split earlier, each half would get its own
  on first touch and never merge again. So in the fallback mode every
  run placed past the mark has its first page resident from the start
  (a whole 2 MiB page for a PMD-aligned run when the host's THP mode is
  `always`), memory the caller would fault in on first touch anyway.
  A `fork` child inherits the splits of the runs live at the fork, and
  the kernel gives each inherited VMA its own `anon_vma` in the child,
  so those may stay split after the child frees the runs: at most two
  VMAs per run live at the fork, never growing with later allocation. A marker the kernel refuses
  (an `mlock`ed range) becomes a `PROT_NONE` page, released the same way
  at free; from then on every allocation also commits its own pages
  (`Huge::mixed_guards`, also set if a release ever fails). Under
  `guards:false` no guard is placed in either mode: the guard page is
  committed with its run and nothing splits the VMA. The cost of the
  fallback is one VMA split at allocation and one merge at free (a
  260 KiB `malloc`, touch and `free` went from 0.8 to 1.2 us with one run
  live and to 2.9 us with a thousand, on Linux 6.18 with the guard advice
  filtered; glibc's `mmap` path is 3.1 us there).
- Alignment above the page is satisfied inside a free extent, at the
  extent's start rounded up to the alignment; the pages around the run
  stay free extents.
- Freed runs become `Empty` and decay like slabs (section 8). Runs whose
  usable size is above `huge_retain_max` (default 64 MiB,
  `huge::DEFAULT_RETAIN_MAX`) are purged at once.
- The huge tier's decay period is arena 0's (`dirty_decay_ms`,
  `arena.0.dirty_decay_ms`) unless `huge_dirty_decay_ms` gives it its own
  (`Shared::huge_decay_ns`, scaled by the cgroup policy like every
  arena's); `0` purges a run as it is freed, whatever its size
  (`Huge::set_purge_on_free`); `-1` leaves freed runs for `malloc_trim(0)`
  and `arena.<i>.purge`. The report's `Huge:` line names the huge period
  when it differs. The separate window is for programs that free whole
  runs in bursts; on the JVM's direct buffers a shorter window bought RSS
  only linearly in the CPU spent refaulting, so the default stays the slab
  period.
- `MADV_HUGEPAGE` on runs of a PMD (2 MiB) or more only under
  `thp:always` (section 17.1); under the default `never` the huge region
  follows the host's policy. A refusal is counted (`stats.thp.failures`).

## 8. Decay and purge

Each `(arena, class)` and the huge allocator keep an `empty` set ordered
by address, with `dirty_at` per entry. Decay policy is jemalloc's in
shape (`purge.rs`, `purge/curve.rs`, `class/decay.rs`):

- `dirty_decay_ms` (default 10000) is the window of jemalloc's decay
  curve: the amount still dirty follows a smootherstep (`6x^5 - 15x^4 +
  10x^3`, `STEPS = 200`, jemalloc's `SMOOTHSTEP_NSTEPS`) over the window,
  so about half of what went `Empty` at once is purged by mid-window and
  all of it by the end. At each step a class's allowance is the sum over
  its `Empty` slabs of the curve's weight at their age, rounded up; the
  oldest are purged until the rest fit and become `Purged` (`resident` to
  `retained`), highest addresses first within the cut-off step. The huge
  allocator does the same in pages over whole extents. A lone slab or
  extent lasts the whole window. A walk runs at most once per step; the
  gate is one `purge::Decayer` (oldest `dirty_at`, next deadline) per
  class state and one in the huge allocator. `-1` is never.
- `muzzy_decay_ms` is accepted and ignored. Two advices instead
  (`policy::Advice`): a purge is `MADV_DONTNEED` (eager: the pages leave
  RSS and the cgroup's charge at once) except under 70% of a known cgroup
  limit (section 11), where it is `MADV_FREE` (lazy: the pages stay mapped
  and charged until the kernel reclaims them, which it does first as the
  cgroup nears its limit, and a reuse before that costs no fault). With no
  limit, the common case, the advice is eager, because lazily freed pages
  stay visible in RSS and mislead autoscalers and Redis's fragmentation
  ratio. Every explicit purge (`malloc_trim(0)`, `arena.<i>.purge`, the
  `high`-rise full purge) is eager, and so is the whole return of an excess
  over the retention cap first seen at 70% or above
  (`Policy::eager_excess`), since its first eager walks bring usage under
  70% before the rest is purged. `lazy_band:false` turns the saving off.
  The advice is read at each batch flush from the band as last evaluated.
- `Purged` memory and zero: `MADV_DONTNEED`ed pages read as zero,
  `MADV_FREE`d pages do not. One process-wide sticky flag
  (`purge::purged_is_zero`), true until the first lazy batch is issued
  and false for good after it, even if the kernel refused that batch
  (published before its discard, under the class or huge lock), tells
  the refill whether a reused `Purged` slab's batch is known zero
  (bookkeeping that `check_zero` reads; no zeroing entry point trusts it,
  section 6.3). A huge run is known zero for pages past the huge
  region's high-water mark, and for a reused `Purged` extent only once
  `Huge::rediscard` has `MADV_DONTNEED`ed the run's pages in it again in
  the lock hold that places the run (for a zeroing caller only, 16 KiB
  or more, and only while `purged_is_zero` holds; otherwise the zeroing
  caller clears it): a stale pointer or an underflow from the live run
  above can have written the extent since its purge. After a lazy batch
  the extent's pages may still be resident, and discarding them would
  fault them back. A caller that does not zero (`malloc`, `realloc`,
  `mallocx` without `MALLOCX_ZERO`) never pays the call.
- Purges are batched (`purge::Batch`): a walk collects up to `MAX_BATCH`
  (64) ranges, merging adjacent ones, and issues a loop of `madvise`.
  Batched `process_madvise` was prototyped and removed: it measured no
  effect (tag `archive/prototype-2026-09-30`).
- A discard can fail: `madvise` refuses `MADV_DONTNEED` and `MADV_FREE`
  with `EINVAL` on a range holding `mlock`ed pages (every range under
  `mlockall(MCL_FUTURE)`). The memory changes state only after its range
  is discarded, so a refused slab or extent stays `Empty` (dirty, its
  age kept, never assumed zero), as jemalloc keeps an extent dirty when
  its purge hook fails. A refused merged range is issued again page
  group by page group, starting with its two ends: when both are refused
  too, the middle groups are left dirty without a call, so a merged range
  under `mlockall` costs 3 calls, not one per group, at the price of a
  range locked at both ends keeping its unlocked middle dirty. One locked
  page in an otherwise unlocked run keeps only its own group. A refused
  call may still have discarded some pages (Linux advises the VMAs before
  the locked one); they stay `Empty` and their bytes are not counted.
  Discarded and refused bytes are counted apart (the report's
  `failed_discards`). Every explicit purge retries; a decay walk that hit
  a refusal holds its owner's walks off for a second
  (`purge::DISCARD_RETRY_NS`) whatever the period, so locked memory under
  a zero period is not retried at every tick. The back-off holds the
  owner's other `Empty` memory too: a class (or the huge tier) with
  locked memory left `Empty` walks once a second, not once a step. Only
  `EINVAL` is a refusal: any other errno is a heap inconsistency and
  aborts, `ENOMEM` (a range not mapped: a reservation is gone) and
  `EPERM` (the program sealed free memory of ours) included.
- A huge extent is one range, refused whole (its unlocked part included).
  It also grows: a run freed next to it coalesces into it as for any
  `Empty` neighbour (the survivor is the older `Empty` extent), and from
  then on the merged extent is refused whole too, so the freed run's
  dirty pages are not returned either, by decay or by an explicit purge,
  until the locked pages are unlocked (the next purge then discards the
  whole extent) or an allocation reuses the extent (the split leaves the
  parts without locked pages to purge on their own). A run purged at
  free time (above `huge_retain_max`, or `huge_dirty_decay_ms:0`) is
  discarded before it coalesces, so only its age and state are lost.
  This only keeps memory resident: an `Empty` extent is never assumed
  zero, a reuse clears what `calloc` asks for, and the extent's pages are
  counted as `Empty`, so the report and `stats.cgroup.empty_bytes` show
  them. The huge decay walk also spends its allowance on the refused
  extent: while it is the oldest, each walk (once a second, the
  back-off) counts its pages against the excess and may skip younger
  unlocked extents, for at most one decay window, until its weight is
  zero. Not absorbing neighbours was not done: it would need a
  "refused" mark per extent, kept through coalescing and splitting and
  cleared by a discard, and it would break the invariant that adjacent
  free extents are always coalesced into one; the growth lasts only as
  long as the lock.
- Under a cgroup limit the period is scaled and the allowance capped by
  the policy (section 11).
- Decay is driven from the slow path (6.4) with a per-class deadline at
  the end of the current step, and from the background thread when
  enabled. Without the background thread an idle process does not purge,
  exactly like jemalloc; Valkey enables the thread through
  `mallctl("background_thread")`.
- `arena.<i>.purge` and `malloc_trim(0)` purge everything immediately.
- `arena.<i>.decay` runs the walks of arena `i` that are due (arena 0's
  include the huge extents) on the periods and keep ratio the driver
  gives them (`purge::decay_arena`): the huge tier's period (section 7)
  and the cgroup policy's scaling (section 11).
  `arena.<MALLCTL_ARENAS_ALL>.decay` runs the driver itself, the owed
  `high`-rise full purge included.

### 8.1 Background thread

Created through `pthread_create` (`purge/background.rs`), never from
init: on the first periodic tick (6.4) after `background_thread:true` in
`malloc_conf`, synchronously by `mallctl("background_thread", true)`
(which returns 0 without waiting when another thread, a tick or another
enable, is creating it at that moment), or,
with the key unset (the default), lazily. The lazy start looks at most
every 100 ms, from a periodic tick whose foreground decay pass left dirty
memory waiting: it sums the `Empty` bytes (every class and the huge tier)
and starts the thread at 64 MiB, or at 4 MiB held at every look for a
second. A process that never holds that much never starts a thread: it
stays single-threaded (`unshare(CLONE_NEWUSER)` keeps working) and its
startup is unchanged (the key is read with the others; nothing else
happens at init). `background_thread:false` (or any write through
`mallctl`) disarms it. A lazy start that fails (a seccomp filter that
fails `clone` with an errno, `RLIMIT_NPROC`, `pids.max`) leaves the
process purging in the foreground as before and is retried no sooner
than a second later, in case a thread limit has freed up; the third
failure disarms it for good. A filter that kills on
`clone` kills the process, so a sandbox that does that needs
`background_thread:false`. Once the thread exists glibc's
`__libc_single_threaded` stays 0, so libstdc++ reference counts turn
atomic and stdio locks, as in any process that ever started a thread.
Only an allocation's tick creates (or joins) the thread, under any
setting: glibc calls `free` with its stack cache lock held
(`pthread_join` frees the TLS of a thread on a user stack, and trimming
the stack cache frees the cached ones), and `pthread_create` takes that
lock again, so a start from that `free` would deadlock. A `free`'s tick
that finds the start due only records the request; the next
allocation's tick acts on it. It
sleeps on a futex until the next decay deadline, is woken when a class's
first slab goes `Empty`, and samples the cgroup (section 11) before each
decay pass. Its stack is one metadata reservation, made by the first
start and never freed: a stopped thread exits at its next wake-up, and
the next start joins it before reusing the stack. Every system call
challoc makes is a raw `syscall`, never a cancellation point; the two
libc calls it makes that are, that `pthread_join` and the `fwrite` of
`malloc_info` to a `FILE *`, run with cancellation disabled: a deferred
`pthread_cancel` of the thread inside `mallctl`, `malloc_info`,
`malloc_stats_print` or an allocation is acted on at the program's next
cancellation point, never midway through a start and never by unwinding
through challoc's frames (14.1). A report's calls out to the program's
own callback, the `write_cb` of `malloc_stats_print` and the
`malloc_message` hook, run with cancellation disabled too, as
`write_file`'s `fwrite` does (`api::print::call_back_nocancel` and
`api::jemalloc::call_hook_nocancel` around `call_back` and
`call_hook`). A callback that leaves the call by `pthread_exit` (in
glibc a forced unwind too) or `longjmp`, or that re-enables
cancellation, breaks premise C3 (`verus/kani/premises.md`).
`operator new`'s calls into the program's `new_handler` (13.1;
`api::cxx::call_handler` and `call_handler_caught`) are the exception:
they run with cancellation enabled. A deferred cancel acted on
inside the handler unwinds through `api::cxx::oom` in the throwing
forms, which is undefined behaviour, and
aborts the process in the `nothrow_t` forms, whose `catch (...)` in
`cxx_handler.cpp` takes glibc's forced unwind without rethrowing it, as
libstdc++'s own nothrow `operator new` does. Until the call runs with
cancellation disabled, premise C4 assumes the handler is not cancelled.
Asynchronous cancellation inside challoc is undefined behaviour
(POSIX allows it only around async-cancel-safe calls) and is not
covered. After `fork` the child
forgets the parent's thread and does not recreate it until asked or,
under the lazy start, until it holds enough dirty memory of its own: the
dirty bytes at the fork are its baseline, lowered as the inherited set
decays (a low-water mark), so what it inherited does not count and what
it frees itself does.
Creating the thread from an allocation's slow path saves and restores
`errno`. The new thread does not keep the scheduling of the thread that
happened to create it (under the lazy start, any allocating thread):
it moves itself to `SCHED_OTHER` with the main thread's CPU affinity
and nice value, as far as the kernel allows (lowering a nice value needs
`CAP_SYS_NICE` or `RLIMIT_NICE`; without either it keeps the nice value
it inherited).

## 9. Fork

`pthread_atfork` handlers registered by the library's constructor
(`api::REGISTER_EARLY`, which does not initialise the allocator: the
handlers do nothing until it is) or, if that did not run first, at init
(`api/mod.rs`). glibc runs prepare handlers in the reverse order of
registration and parent and child handlers in order, as POSIX says, so a
handler registered after challoc's (by the program's constructors or
code, or a library initialised after challoc) runs before challoc's
prepare takes the allocator's locks and after its release: it may
allocate, or wait on threads that allocate (a thread pool's shutdown,
`tests/c/atfork_pool.c`), as under glibc's malloc. The handlers
registered before challoc's (from the program's `.preinit_array`, or by a
library whose constructor ran before challoc's, as LD_PRELOAD and
DT_NEEDED order decide) run on the forking thread while challoc's hold
every lock. One of those that waits on another thread which allocates
still deadlocks `fork` (a residual, with `_Fork` below); one that
allocates itself does not. That residual is the common deployment: under
LD_PRELOAD, glibc runs every DT_NEEDED library's constructor before the
preloaded challoc's, so a program linked against a thread-pool library
that registers such a handler from its constructor (libopenblas,
libgrpc) and run with challoc preloaded deadlocks in `fork`. Only a
challoc linked into the program (or preloaded into a program whose pool
registers later, from its own code) runs its prepare after the pool's. Between the end of challoc's prepare handler
and the start of its release, that thread is the fork window's holder
(`lock::fork_window_open`): a lock it asks for is taken as acquired and
its release does nothing (`lock::fork_holder_is_me`, checked only in the
contended acquire and in the release), so an allocation in such a
handler proceeds instead of waiting on its own thread
(`tests/c/atfork_early.c`). The premise, trusted: the forking thread has
no allocator critical section open in the window, since the prepare
handler took every lock with every signal blocked, so each lock's data is
as its last holder left it and only that thread runs allocator code
until the release. What the fork handlers snapshot or reset may not
change in the window: the background thread neither starts nor stops
there (`mallctl("background_thread")` is refused with `EAGAIN` and
changes nothing, and the lazy start's `start_requested` and `start`
refuse there, so it waits for a tick after the fork;
`tests/c/fork_window.c`, `tests/c/fork_window_lazy.c`), since the child handler forgets a thread on
the premise that none can exist yet; and no arena is built there
(`arenas.create` is refused with `EAGAIN`; an allocation whose home
arena is not built yet is served from arena 0, the thread's next slow
path after the fork building it; and an explicit request for an
automatic arena not built yet, `mallocx` with `MALLOCX_ARENA(i)` or a
`thread.arena` write, fails as for an index that does not exist: a
consequence of the rule, not a contract, and no test pins it), so every lock in the process is one
the prepare handler took, the only kind the window serves, and the
registry the release reads is the one it locked. A cgroup discovery or a feature
line built in the window completes before the fork, and their child
resets undo only an unfinished one. A handler in the child that reads
the cgroup figures (`stats.cgroup.*`) or the feature line
(`malloc_stats_print`) while another thread of the parent was
discovering or building it at the fork does not wait for work that will
not finish: in the child's window (`lock::in_child_fork_window`, the
holder with another process ID than the one recorded when the window
opened) the wait resets that work as the child handler would and does it
itself (`tests/c/fork_spin.c`). A `fork` never copies an init under way
on another thread (the gate, `api::INIT_GATE`): an init opens by an
exchange that writes its thread's `pthread_self` into the gate, and only
that thread goes on to claim `STATE` (others wait for `GATE_DONE`); the
prepare handler, before it looks at the state, waits for an init another
thread has under way to publish, so the child gets the finished state;
with none under way it closes the gate (`GATE_FORKING`), so that no init
starts until the parent and child handlers open it again, and a child
whose parent was not initialised starts uninitialised. It initialises nothing
itself, so a process that never allocates stays free of init. The
forking thread's own init (a handler registered before challoc's that
allocates, in the parent's prepare or the child's handlers, which run
before challoc's opens the gate) passes it: the gate knows that thread
by `pthread_self`, which it keeps in the child (`tests/c/fork_init.c`,
`tests/c/fork_first.c`). Residuals: a handler registered before
challoc's (above) that waits on another thread which allocates deadlocks
`fork`, as that thread waits on a lock challoc's prepare holds (above:
the preloaded-challoc, linked-pool case); a `fork` from a signal handler
that interrupted its own thread's init (the gate word names that thread,
so the parent's prepare does not wait and the parent finishes its init
when the handler returns), whose child inherits the half-built state;
in a process where challoc is loaded but not initialised and threads
already run (loaded late by `dlopen`, or never allocated through), a
prepare handler registered before challoc's that takes a lock L while a
thread Y holds L and makes its first allocation: Y waits at the closed
gate and the forking thread waits for L (introduced by the gate, and only
while challoc is uninitialised with threads running); a `fork` whose forking thread initialises challoc from
a prepare handler registered before challoc's: `STATE` is `READY` from
then on while challoc's prepare took no lock, so another thread's first
call (which no longer reaches the gate) may hold an allocator lock that
the child inherits held, and the child's handler, which released
nothing, leaves it so; and a
process created inside the window by `_Fork` or a raw `clone` inherits
every lock held with its only thread as the holder, so its main thread
proceeds but a thread it creates waits forever on its first slow path. The prepare
handler blocks every signal on the forking thread, then takes every lock
in nesting order: the explicit-cache table lock (outermost: it is held
across heap creation and destruction), the arena creation lock, the
process statistics lock (held across every statistics scan, which
takes the class, slab table, huge, metadata and per-arena statistics
locks, so one `epoch` collection is published at a time), every arena's
class locks, the slab table locks, every arena's slab-set lock (taken
under a class lock and its slab table lock when a group is handed
out), the huge lock, the metadata lock, the per-arena statistics locks
(leaves), and the background thread's state lock last. The parent and
child handlers release them in reverse and restore the signal mask. The
test `fork_lock_list_matches_the_source_tree` derives the list of
`Lock` fields from the source tree and fails until a lock added later
is in the tests' `FORK_LOCKS` list, and
`fork_handlers_take_and_release_every_lock` fails until the handlers
take it. The signal blocking is needed because
glibc restores the caller's mask before the parent handlers run, so
a signal handler that forks there (glibc's `tst-mallocfork`) would
re-enter prepare with the locks held. Other threads' heaps are left
alone in the child; their cached slots stay marked and are reclaimed
when the child exits, as with every other allocator.

## 10. Initialisation and kernel feature detection

Init is lazy, triggered by the first call to any entry point, and
serialised with an atomic state machine (`Uninit -> Initialising ->
Ready`; a second thread spins). ld.so may call `malloc` before
constructors run, so nothing relies on `.init_array`. Init must not call
anything that allocates. Steps, in the order `api::init` performs them:

1. Adopt the page geometry from `AT_PAGESZ` (section 3), then parse
   configuration (`config::load`), before anything is mapped or
   reserved, so `abort_conf` aborts with nothing mapped.
2. Probe (`Features::probe`): `MADV_GUARD_INSTALL`, `mseal` and
   `PR_SET_VMA_ANON_NAME` tried on a scratch page; the THP sysfs table
   (`sys/thp_host.rs`: `enabled`, `hpage_pmd_size` and, unless `thp` is
   `never`, every `hugepages-<n>kB/enabled` from two pages to the PMD,
   `inherit` resolved through the top level; before 6.8 only the PMD size
   exists); glibc's rseq registration (`rseq::probe`); pointer
   authentication from `AT_HWCAP` (`sys::pac`). Under `thp:never` the
   per-size files are read only when the feature line is first asked for
   (step 8).
3. Decide the THP mode (`thp::configure`), configure the purge advice from
   `lazy_band`, set `check_zero`, record a `background_thread` request.
4. Reserve the three regions (`Regions::reserve`: `getrandom`, size
   probing, random order, `PR_SET_VMA_ANON_NAME` names).
5. With `mseal=yes`, seal the never-committed ranges (`Regions::seal`, at
   most `MAX_SEALED` = `NCLASSES + 5` calls). A refusal stops the sweep
   without aborting (sealing is a hardening extra, not a heap invariant);
   the report says `Sealed ranges: N of M`.
6. `Shared::init`: the slab tables, arena 0, the huge allocator, the
   registries and the thread-exit key.
7. Inside `Shared::init`, cgroup discovery under `thp:always` only
   (section 11, `Cgroup::discover`, skipped with `cgroup:false`), then
   `thp::force_never` if the cgroup rule forced `never`; under any other
   `thp` key discovery is deferred (`sys::cgroup::Deferred`, section 11).
8. Register the fork handlers (section 9). The feature line for
   `challoc_features()` and `malloc_stats_print` is formatted at its first
   request (`api::feature_line_ready`), not here.

Every step is a fixed cost of every exec, so what a short-lived process
never needs waits for first use: the cgroup files, the per-size THP files,
and the commit of the small tables carved from the metadata object side
(section 2, one `mprotect` for all of them). `startup-true` (`/bin/true`,
its `exec` included) makes 234 system calls under challoc on ref-amd64,
against 357 before these deferrals and 157 under jemalloc 5.4.0 (measured
before the `RLIMIT_DATA` probe and the two reservation placeholders of
section 2, which add up to five).
Slabs are still committed one at a time as the arena reaches them
(section 5): committing a group ahead would leave the slab after an
arena's newest one read-write, where an overflow now faults.

A pointer passed to `free` before init completes, or that does not
classify, is invalid and aborts with a message. ld.so's minimal startup
allocator only frees its own last block, so nothing foreign reaches the
allocator in practice.

## 11. cgroup awareness

The measurement layer is `src/sys/cgroup.rs`, the policy that acts on it
`src/policy.rs`. Everything is gated by the `cgroup` key (default `true`;
`cgroup:false` reads nothing, applies nothing and reports `cgroup: not
tracked (cgroup:false)`).

Discovery, once (section 10 step 7). Under `thp:always` it runs at init,
which needs the limit for the 1 GiB rule below; otherwise it is deferred
(`sys::cgroup::Deferred`) to the first periodic tick at least `DEFER_NS`
(20 ms) after init, to the first read of the figures (`stats.cgroup.*`,
`malloc_stats_print`) or to the first `fork` (the prepare handler, so
the children of a forking server inherit the record rather than each
reading the files again), whichever comes first, so a process that lives
less than that never reads the files (about 90 us of system time per
exec). Whichever thread runs it, the record is built in place in its
cell with the scan buffers in a static the discovering thread owns, so
the discovery takes little of that thread's stack (a `malloc` on a
`PTHREAD_STACK_MIN` thread may be the one that runs it). Until then the
policy sees no record: no limit known, the policy
at its defaults, the eager purge advice; a reader of the figures (the
report, `stats.cgroup.*`) discovers first, so it never shows a pending
record. The `memory.events` `high` count found at discovery is the
policy's baseline, as it is at init, set before the record is published
so no other thread's sample sees the record without it. A `fork` does not
wait for a discovery another thread has under way; the child starts that
one over.

- The cgroup path is the `0::` line of `/proc/self/cgroup` (`/` inside a
  cgroup namespace); the cgroup2 mount is the first `cgroup2` line of
  `/proc/self/mountinfo`, normally `/sys/fs/cgroup` (a hybrid host's
  `/sys/fs/cgroup/unified` works the same way). Without either (a v1-only
  host) nothing is tracked. Paths are capped at 2 KiB and files are
  scanned through 2 KiB stack buffers.
- Effective limits: walk from the leaf up to the mount, inclusive,
  reading `memory.max` and `memory.high` at each level and keeping the
  minimum per file; `max` and an absent file mean unlimited. The walk
  also keeps each level's own `min(high, max)` for the 16 levels nearest
  the leaf (`LEVELS`). It is repeated at most once per second
  (`LIMIT_INTERVAL_NS`), so a `docker update --memory` is seen.
- CPU limits (`cpu.max`, `cpuset.cpus.effective`) are not read. Sizing
  the arena count from the CPU quota was prototyped and removed: neither
  `narenas` = quota nor 4 x quota held throughput (tag
  `archive/prototype-2026-09-30`).

All reads use raw `open`/`read`/`close` through `src/sys`, never libc
stdio, and nothing allocates.

Sampling: `memory.current` and `memory.events` (`high` and `max`, with a
count of the samples that saw each rise) are read at most every 10 ms
(`SAMPLE_INTERVAL_NS`, `CLOCK_MONOTONIC_COARSE`; one compare-exchange on
the timestamp elects the reader), with `memory.current` of each ancestor
within `LEVELS` that has a limit of its own (one more read per such
ancestor; usually none inside a cgroup namespace, where the process
mostly sits at the mount), from the periodic tick of the slow path
(`purge::on_tick`), the background thread's loop, a
`stats.cgroup.current` read and `malloc_stats_print`; the fast paths
never see it. A caller's `now` older than the last sample is not due, so
the window only moves forward. A `memory.current` read that fails keeps
the last figure, which the `cgroup:` line marks `(stale)` until a read
succeeds. A PSI trigger on `memory.pressure` was prototyped and removed:
it measured no effect over the tick (tag
`archive/prototype-2026-09-30`).

Policy. It is evaluated where a sample was taken, so at most once per
10 ms process-wide; the outcome is a few atomics on `Shared` (the band, a
keep ratio, a pending full purge) that the decay driver and the periodic
tick read. Usage is the last `memory.current` sample against the
effective limit `min(high, max)`, and each ancestor's own usage against
its own limit; the most severe row wins. The kernel holds an ancestor to
its limit by the usage of all its children, so a parent limited to
1000 MiB that holds 950 MiB puts this process at 95% even when the leaf
holds 100 MiB of it. The leaf's row also stands in for limited levels
beyond `LEVELS`:

| Usage vs limit (worst level)      | Behaviour                                                  | Purge advice (section 8) |
|-----------------------------------|------------------------------------------------------------|---------------------------|
| No limit                          | Defaults; nothing beyond the sample is done                | `MADV_DONTNEED`           |
| < 70% of `min(high, max)`         | Defaults, retention capped at 5% of the limit              | `MADV_FREE` (`lazy_band`) |
| 70% .. 90%                        | `dirty_decay_ms` scaled down linearly to 1000; the cap     | `MADV_DONTNEED`           |
| >= 90%                            | Period 0: purge at the next pass; cache GC every tick      | `MADV_DONTNEED`           |
| `memory.events` `high` increased  | Full purge once, at the next decay pass                    | `MADV_DONTNEED`           |
| < 70% while an excess over the cap first seen at 70%+ is still being returned | as the `< 70%` row | `MADV_DONTNEED` until the retention is within the cap |

The thresholds are `SCALED_FROM_PPM` (70%), `IMMEDIATE_FROM_PPM` (90%),
`FLOOR_NS` (1000 ms) and `RETENTION_DIVISOR` (5%).

*Retention* is the `Empty` memory of every class of every arena and of
the huge allocator: freed, dirty, not yet purged, the only memory the
allocator holds that costs RSS without serving anything. The cap is
applied inside the curve walks of section 8 as a keep ratio `cap /
retention`: a walk keeps the smaller of the curve's allowance and that
share of its `Empty` slabs or pages, so the curve still decides what goes
first. The huge walk never splits an extent and overshoots rather than
staying over the cap. The sum is taken by the evaluator, under each class
lock, for classes that have something `Empty`, only when a limit exists.
The cap applies in both bands under 90%; at 90% and above the zero period
empties everything at the next decay pass anyway.

Scaling the period scales the curve's window: at 70% the configured
`dirty_decay_ms`, just under 90% 1000 ms, linear between; a period already
at or under 1000 ms is left alone, and `-1` (never) is left alone in every
band as an explicit choice. A rise of `memory.events` `high` (the kernel
started reclaiming from this cgroup) requests one full purge, run by the
next decay pass; the count at init is the baseline. A limit that goes
away resets the outcome to the defaults.

Lazily returned pages stay charged to `memory.current` until the kernel
reclaims them, so the sample counts them and the band moves toward 70%
sooner than live memory alone would. That flips the advice to eager while
memory is plentiful, an error in the safe direction, and the lazy pages
are the first thing the kernel reclaims near `memory.high` or
`memory.max`, so they never stand between the program and its limit.

The policy acts at a decay pass, and without a background thread a decay
pass runs only from the periodic tick, so a process that frees memory and
then allocates nothing at all keeps it under every setting. The policy
discharges retention, not live memory: at the top of a climb the peak is
`allocated` plus the slack of partially used slabs plus metadata, and no
band can take those back. The container tests (`tests/c/cgroup_churn.c`,
`cgroup_oscillate.c`, `cgroup_lazy.c`, `cgroup_policy.c`;
`scripts/cgroup-tests.sh`, `make test-cgroup`) exercise each row.

THP under small limits: `thp:always` is forced to `never` at init when
the effective limit is under 1 GiB (`policy::THP_MIN_LIMIT`,
`forced_by_limit`; `opt.thp` then reads `never` and the report says why):
a 2 MiB page is the unit a small container cannot afford to have
partially used. `thp:auto` is not forced: its 64 KiB folio is the slab's
own granularity. A forced `never` is a `never` (`thp::force_never`,
section 17.1).

Statistics through `mallctl` (13.4), all `size_t` and read-only:
`stats.cgroup.limit`, `.high` (`SIZE_MAX` for unlimited), `.current` (the
last sample, refreshed if the window allows), `.decay_ms` (arena 0's
effective period, `SIZE_MAX` for never), `.retention_cap`, `.empty_bytes`
(summed by the read) and `.full_purges`, `ENOENT` when nothing is
tracked; and, readable whether or not a cgroup is tracked,
`.purge_advice` (1 lazy, 0 eager), `.lazy_purges` and `.eager_purges`
(the batches with each advice that discarded anything: a batch the
kernel refused whole is not counted, nor are the bytes a refused call
discarded before it stopped). `malloc_stats_print` prints `cgroup:`,
`cgroup policy:` and `purge advice:` lines (section 16).

## 12. Configuration

`src/config.rs` is the source of truth for the key list, the defaults and
the value grammar: one table (`TABLE`) holds every tunable with its
name, parser and reader, and `KEYS` (the `opt.` namespace), `Config::set`
and `Config::get` all come from it. [tuning.md](tuning.md) is the
operator's reading: one row per key with its values, when to change it
and what it costs.

Sources, in increasing priority (`config::load`):

1. Compiled defaults (`Config::DEFAULT`).
2. The `malloc_conf` global (the library defines a null `malloc_conf` and
   reads it through the GOT, so a program's own definition interposes),
   then the weak `je_malloc_conf`, jemalloc style. Redis defines
   `je_malloc_conf` with `lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000`.
3. `/etc/challoc.conf`, first line, if present. Read with raw syscalls.
4. `MALLOC_CONF`, then `CHALLOC_CONF`, read from `environ` without
   `getenv`; both skipped when the process is `AT_SECURE`.

Format is jemalloc's `key:value,key:value`. The keys, in `TABLE` order:

| Key                       | Default | Notes                                   |
|---------------------------|---------|-----------------------------------------|
| `background_thread`       | unset   | unset starts lazily, reads `false`; 8.1 |
| `dirty_decay_ms`          | 10000   | `-1`..`1000000000000`; 8                |
| `muzzy_decay_ms`          | 0       | stored and readable; nothing acts on it |
| `narenas`                 | 0       | 0 = `min(4 * ncpus, 64)`; 13.3          |
| `percpu_arena`            | disabled | `disabled`, `percpu`, `phycpu`; accepted for jemalloc compatibility, no effect (section 15) |
| `tcache`                  | true    | false disables caches                   |
| `tcache_max`              | 32768   | largest cached class size (bytes), `0`..`262144` |
| `tcache_nslots_small_max` | 1024    | `8`..`65536`; 6.2                       |
| `lg_tcache_nslots_mul`    | 1       | `-16`..`16`; 6.2                        |
| `thp`                     | never   | `never` (alias `default`), `auto`, `always`; 17.1 |
| `lazy_band`               | true    | `MADV_FREE` under 70% of a known limit; 8, 11 |
| `guards`                  | true    | guard slabs and huge-run guards; 5, 7   |
| `guard_slab_interval`     | 8       | one guard per N x 64 KiB of slabs; 0 off, 1 clamps to 2, a non-power of two rounds up; max 2^22 |
| `huge_retain_max`         | 67108864 | bytes; freed huge runs above it are purged at once; 7 |
| `huge_dirty_decay_ms`     | follows `dirty_decay_ms` | the huge tier's own window; 7 |
| `zero_on_free`            | false   | 6.3 step 3                              |
| `pac_pointers`            | false   | arm64 with PAC only; 6.1, 6.2           |
| `slot_randomize`          | false   | 4                                       |
| `double_free_scan`        | false   | 6.3 step 3                              |
| `exact_sized_free`        | false   | sized frees run `free`'s check; 6.3     |
| `abort_conf`              | false   | abort on unknown keys, bad values and inert keys |
| `cgroup`                  | true    | 11                                      |
| `check_zero`              | false   | debug: verify every slot or page handed out as zero |

Two more lists (`config::INERT_KEYS`, `config::IGNORED_KEYS`):
`stats_print` is *inert*, parsed as a bool and reported every time it is
set (so `abort_conf` refuses a configuration that relies on it and
`malloc_stats_print` marks it "accepted, no effect"; nothing prints at
exit); `junk`, `zero`, `retain`, `oversize_threshold`,
`metadata_thp` and `confirm_conf` are jemalloc keys accepted with any
value and ignored (`metadata_thp` because the metadata follows `thp`).
Unknown keys, bad values, out-of-range numbers (clamped), inert keys and
sources longer than 4096 bytes (the pair at the cut is dropped) are
reported on stderr and otherwise ignored unless `abort_conf` is set,
which is evaluated after every source has been read. `opt.<key>` is
readable through `mallctl` for every key of the table; the inert and
ignored keys return `ENOENT`.

## 13. API surface

### 13.1 Standard and glibc

`malloc`, `free`, `calloc`, `realloc`, `aligned_alloc`, `posix_memalign`,
`memalign`, `valloc`, `pvalloc`, `free_sized`, `free_aligned_sized`,
`reallocarray`, `malloc_usable_size`, `malloc_trim` (`api/libc.rs`);
`mallopt` (accepts and ignores every parameter, returns 1), `mallinfo`,
`mallinfo2` (`api/glibc.rs`); `malloc_stats` and `malloc_info` (a minimal
XML document with totals; `api/print.rs`). Every export is a top-level
`pub extern "C"` function with `#[unsafe(no_mangle)]` or
`#[unsafe(export_name)]`, since rustc's version script only exports those.

The C++ operators are exported too, as jemalloc does (`api/cxx.rs`): the
eight `operator new` and twelve `operator delete` forms under their
Itanium ABI names, which saves a C++ program under `LD_PRELOAD` a second
PLT hop through libstdc++ on every allocation. `operator new` follows the
standard failure protocol (call `std::get_new_handler()` while one is
installed, then `std::__throw_bad_alloc()`; the `nothrow_t` forms return
null; an `align_val_t` that is not a power of two fails at once, without
the handler, as in libstdc++). Both libstdc++ entry points are weak
references loaded from the GOT, so a C process that never maps libstdc++
still loads under `BIND_NOW`, and a program whose static libstdc++ hides
them gets an abort with a message instead of the throw. The `nothrow_t`
forms also return null when the handler throws: they call it through
`challoc_call_new_handler` (`api/cxx_handler.cpp`, compiled by build.rs),
a C++ `catch (...)` whose runtime references (`__cxa_begin_catch`,
`__cxa_end_catch`, `__gxx_personality_v0`) are weak too. The throwing
forms call the handler directly, so its exception reaches the caller
unchanged. The sized `delete` forms are `free_sized` and
`free_aligned_sized`: the size and alignment name a class through the
same `class_for(size, align)` the matching `new` used (or, for a huge
run, its page count), and a mismatch aborts (jemalloc's sized `delete` is `sdallocx` with the same contract).
The unsized aligned forms take the plain `free` path.

### 13.2 jemalloc

Symbols (`api/jemalloc.rs`): `mallocx`, `rallocx`, `xallocx`, `sallocx`,
`dallocx`, `sdallocx`, `nallocx`, `mallctl`, `mallctlnametomib`,
`mallctlbymib`, `malloc_stats_print`, each also as `je_<name>`;
`malloc_conf` (a data symbol, defined null and exported so a program that
only references it links, as Ruby's configure probe does; `je_malloc_conf`
is only read, as a weak reference); `malloc_message` (a function pointer,
default writes to stderr; `je_malloc_message` is consulted when it is
null).

`MALLOCX_*` flags decode as jemalloc's (`heap/mallocx.rs`): bits 0-5
`lg_align`, bit 6 `ZERO`, bits 8-19 `tcache` (0 default, 1 none, `t+2`
explicit cache `t`), bits 20-31 `arena` (0 default, `a+1` arena `a`).
`MALLCTL_ARENAS_ALL` is 4096.

`sdallocx(p, size, flags)` requires `nallocx(size, flags) == sallocx(p)`,
as jemalloc documents: the class that `size` and `MALLOCX_LG_ALIGN` name
is checked against the pointer's class and a mismatch aborts (`challoc:
sized free of ...: size hint N does not match the allocation`); a huge
run takes any size whose page count is the run's (any size with the run's
`nallocx`, and the small size of a run an alignment made huge), and
another size aborts the same way; the slot
bit is not read, so an invalid or double free through `sdallocx` is
left to the cache flush, which catches it only if the slot is still
unclaimed then (6.3); a slot start in a guard slab is rejected at the
call. `dallocx` is `free` with flags. C23 `free_sized` and
`free_aligned_sized` follow the same rule; `free_sized` on memory from
`aligned_alloc`/`memalign` is undefined in C23 and aborts here when the
alignment put the block in a larger class.

### 13.3 Arenas and explicit caches

An arena (`arena.rs`) is a set of `ClassState`s over the shared
`SlabTable`s, with its `SetTable`, its decay period and its statistics
(the huge allocator is shared by every arena). Arena 0 exists at init;
`arenas.create` adds one. A slot belongs for good to the arena that owns
its slab group (section 4): refills claim from the home arena and flushes
release each slot to its slab's arena (`release_routed`), so per-arena
statistics stay exact whichever cache a slot passes through, and Redis's
Lua arena keeps its accounting through `MALLOCX_ARENA`. An `Empty` slab is
reusable only by its own class and arena, where jemalloc's one dirty pool
per arena serves every size; that is the price of per-class regions,
visible as a higher peak RSS on many-arena microbenchmarks.

`narenas` shards automatic allocations across arenas by thread, jemalloc
style. The default is `min(4 * ncpus, 64)` (`Config::effective_narenas`;
`ncpus` from the affinity mask). A new thread heap is bound to the
automatic arena with the fewest bound heaps (jemalloc's
`arena_choose_hard`); the binding is released when the heap is destroyed
and moves with `thread.arena`. The arena is built at the heap's first
slow path; while it cannot be built (the metadata region is full: each
arena takes about 150 MiB of reservations, so about 437 fit the
preferred 64 GiB region at quantum 16; or `RLIMIT_DATA` refuses its
commits) the thread's blocks come uncached from arena 0 and
every later slow path retries the build (14.1). A build is not started
when the free metadata is below what a build takes (the smallest
measured, plus up to 16 MiB of slack for concurrent reservations): an arena's
tables are reserved one after another and never given back, so a build
that ran out partway would strand them, leaving the huge allocator no
room for its tables. A thread served from arena 0 still takes the
creation lock and reads the metadata counters on each slow path to try
the build; that is the cost of retrying without remembering a refusal.
`arenas.create` keeps room for up to eight automatic arenas not built
yet (`AUTO_HEADROOM`, at that cost each) and
refuses with `EAGAIN` before it would use it, so explicit arenas never
take every thread's home; homes past those share arena 0. So `t` live
threads use `min(t, narenas)` arenas, a single-threaded program stays on arena 0 and never pays the
`free` routing (`FREE_ROUTE` is set only when a second arena is built),
and thread churn reuses the arena an exited thread left. Arenas are not
needed for correctness or lock scalability (slabs are not thread-owned)
but for cache locality: under one shared arena every refill interleaves
the threads' slots in the same slabs, and 10-thread `larson` paid about
one L1D miss per operation for it. Classes above `tcache_max` are the
exception: uncached, with 1..4 MiB slabs, they are allocated from arena 0
whatever the thread's arena (`Config::shared_class`), because a partial
slab per class per arena was tens of MiB of RSS; `MALLOCX_ARENA(a)` keeps
`a`.

`tcache.create` allocates a `Heap` owned by no thread
(`heap/tcaches.rs`); `MALLOCX_TCACHE(t)` uses it. Explicit caches are not
thread safe, matching jemalloc. `MALLOCX_TCACHE_NONE` claims or releases
directly against the slab bitmaps: a direct claim lands in the arena's
lowest-address partial slab, where jemalloc's `slabcur` puts the same
request, which is what Valkey's defragmenter needs (its copies land
outside the sparse slabs it is emptying once the low slabs are packed).

### 13.4 `mallctl` namespace

Implemented keys (`api/mallctl.rs`), chosen from what Redis, Valkey,
Ruby, Aerospike, Rspamd, ClickHouse, MariaDB, RocksDB and
tikv-jemallocator actually call:

```
version, epoch, background_thread, max_background_threads
opt.<every key of section 12's table>
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
stats.cgroup.limit, stats.cgroup.high, stats.cgroup.current,
stats.cgroup.decay_ms, stats.cgroup.retention_cap, stats.cgroup.empty_bytes, stats.cgroup.full_purges
  (section 11; ENOENT when no cgroup2 is tracked or cgroup:false)
stats.cgroup.purge_advice, stats.cgroup.lazy_purges, stats.cgroup.eager_purges
  (section 11; readable without a tracked cgroup)
stats.thp.mode, stats.thp.class_ranges, stats.thp.class_bytes, stats.thp.huge_ranges,
stats.thp.meta_ranges, stats.thp.nohuge_ranges, stats.thp.failures (section 17.1)
```

An `epoch` write runs the calling thread's cache GC first when one is due
(6.4, refresh GC), so the snapshot it publishes does not count slots that
only sit in that thread's caches; a read refreshes nothing.

Everything else, `prof.*` included, returns `ENOENT`.

Semantics that differ from jemalloc's, deliberately: `arena.<i>.destroy`
flushes and purges the arena and leaves it registered and usable
(`EFAULT` for arena 0 and `MALLCTL_ARENAS_ALL`); challoc never destroys
an arena, so `stats.arenas.4097.*` (`MALLCTL_ARENAS_DESTROYED`) answers
all-zero statistics and `arena.4097.*` `ENOENT`. Reading `thread.arena`
builds the calling thread's heap if it has none (`EAGAIN` while a heap is
being created or torn down); writing it to an unbuilt automatic arena
builds that arena. `opt.tcache_max` and `opt.huge_retain_max` read back
as `size_t`, the other unsigned keys as `unsigned`, the signed ones as
`ssize_t`, the enumerated ones (`opt.thp`, `opt.percpu_arena`) as
strings.

`arenas.nbins` counts the classes up to 4 KiB (`BIN_MAX`; 32 with
`quantum8`, 28 with `quantum16`); the larger classes are slab-allocated
all the same but appear the way jemalloc reports large extents (in
`large.*`, absent from `bins`, `(nfree 0, nregs 1, size)` from the
utilisation queries), because Valkey's defragmenter and its
`frag_smallbins` metric are calibrated to jemalloc's few-page bin slabs,
and a 256 KiB slab pinned by a few live 10 KiB blocks would count as
fragmentation no defragmenter can recover. The utilisation queries
report `size` as `nregs * size`, so `size / nregs` is exactly the class
size, as Valkey asserts.

`epoch` merges every heap's pending statistics into arena totals
(`stats.rs`) and publishes the arenas and the process totals as one
collection, under the process statistics lock (section 9), as jemalloc's
refresh does under `ctl_mtx`. `stats.allocated` is the sum over classes
of `live * size` plus huge usable bytes; `active` counts committed slabs and huge runs that
are not `Empty` or `Purged`; `resident` is `active` plus `Empty` bytes
plus committed metadata; `retained` is `Purged` bytes; `mapped` is
`resident + retained`.

`experimental.utilization.batch_query` fills `(nfree, nregs, size)` per
pointer in jemalloc's layout, and the verbose `query` adds `(bin_nfree,
bin_nregs, slabcur_addr)`; `nfree` is `nslots - live` and `slabcur_addr`
the lowest partial slab of the class in the owning arena. Valkey's
defragmenter reads these and `stats.arenas.4096.bins.<j>.{curregs,
curslabs, nonfull_slabs}`, moves an object whose slab is more than 12.5%
below the average utilisation of the bin's non-full slabs, and runs only
while `frag_smallbins` is at least `active-defrag-ignore-bytes`; the rule
is Valkey's. challoc's `nonfull_slabs` includes the slab a claim takes
from next, where jemalloc's excludes `slabcur`, so challoc reports one
more non-full slab and moves slightly more.

### 13.5 Redis-specific extensions

`get_defrag_hint(void *ptr)` and the `*_with_usize` family exist only in
Redis's patched bundled jemalloc; Valkey uses the standard
`experimental.utilization` keys (13.4). `get_defrag_hint` is exported
(both spellings; the shipped headers declare it and define
`JEMALLOC_FRAG_HINT`, from which KeyDB's `zmalloc.h` derives
`HAVE_DEFRAG`) because KeyDB, a Wolfi package, calls `je_get_defrag_hint`
and has no other defragmenter input. It answers 1 by the rule of Redis's
`iget_defrag_hint`, from the same data as the utilisation queries: the
pointer is a live slot of a slab class, its slab has a free slot, the
slab is not the one a `MALLOCX_TCACHE_NONE` claim takes from next, and its
live count is at or below the average of the class's other slabs in that
arena (`live * (curslabs - 1) <= curregs - live(slabcur)`). Every slab
class answers, the ones above `BIN_MAX` included, since the hint carries
no geometry. It answers 0 for everything else (a huge run, a full slab, a
freed or foreign pointer, null); a wrong answer only costs the
defragmenter a move, and nothing about the pointer aborts.

Before it answers, the hint runs the calling thread's cache GC when one
is due (`Heap::gc_for_defrag`, 6.4). Free slots still in a cache are neither
live data the defragmenter can move nor free in their slabs: they pin
their slabs, and Redis's fragmentation figure (`frag_smallbins`, unused
slots of current slabs) and KeyDB's stop rule (`stats.active -
stats.allocated`) both count them. Redis 8 raises the cache to 1000 slots
a class (`je_malloc_conf`), and with only the periodic GC a Redis defrag
pass that finds nothing worth moving rescans its keyspace until the next
10 s GC. The call counts in `hint_ticks`, apart from the slow path's
`slow_ticks` so hints neither take the periodic tick's turns nor run down
the rehome window, and reads the clock (a system
call, `now_coarse_ns`) once every `PERIODIC_EVERY` calls, since a
defragmenter asks about every allocation it scans and a clock read per
call would put a system call on every hint. When the read finds the last
GC at least `DEFRAG_GC_INTERVAL_NS` (100 ms) old, `Heap::gc` runs: it
releases only the entries that went unused since the last GC. The release is the free path's (`Heap::flush`), so it can abort
where a valid program's free can: at a failed system call or a pending
free-path check (`valid_aborts`). With the thread-heap lookup's
integrity check (`may_abort_corrupt`), those are all the hint requires,
not `may_abort`: `theorem_defrag_hint_aborts_only_in_its_gc` shows it
aborts at most there, for every pointer, and none of those checks reads
the pointer. It is not among the read-only entry points of
`theorem_read_only_never_aborts`, which allow only integrity checks. The
`*_with_usize` family stays out of scope: no Wolfi package calls it.

### 13.6 Library name, soname and packaging

`cargo build --release` writes `libchalloc.so` and `libchalloc.a`. The
shared library's soname is `libchalloc.so.0` (the ABI major; the crate
version is 0.x); a package installs the file under that name with a
`libchalloc.so` link for the linker (`scripts/install.sh`). With the
`jemalloc-soname` feature the soname is `libjemalloc.so.2` instead,
staged by `scripts/jemalloc-compat.sh`. The static library is built but
not yet tested or shipped.

One quantum per build (section 3): a run-time quantum would put a table
select on every class lookup. Wolfi's `jemalloc` has quantum 16, so the
system `libchalloc.so.0` and the `challoc-jemalloc-compat`
`libjemalloc.so.2` are `quantum16` builds (Ruby runs on the latter).
Valkey is the one `quantum8` consumer (its defragmenter asserts
`arenas.quantum == 8`) and gets a private `quantum8` build in its own
package, under `/usr/lib/valkey/`, named in the binary's rpath
(`contrib/wolfi/valkey/valkey-9.1.yaml`); `DT_RPATH` is searched before
`LD_LIBRARY_PATH`, so only `LD_PRELOAD` can displace it. Two libraries
with one soname cannot share `/usr/lib`, so the system library is never
`quantum8`.

`DT_NEEDED` under `--as-needed` (Wolfi's gcc default): the linker keeps a
library only if it resolves an earlier reference, so an executable that
calls no allocator symbol drops `libchalloc.so.0`. In the two-level shape
(Ruby: the `ruby` binary allocates only through `libruby.so`) the loader's
breadth-first order then puts `libc.so.6` ahead of `libchalloc.so.0` and
`malloc` binds to glibc for the whole process, with nothing failing. A
package therefore links with `-Wl,--push-state,--no-as-needed -lchalloc
-Wl,--pop-state` (what the shipped `challoc.pc` does) and runs
`scripts/check-needed.sh <binary> libchalloc.so.0` (`readelf -d` only) in
its package test on every executable and shared library that must run on
challoc. `tests/as-needed.sh` and the consumer builds in
`tests/consumers/` exercise it (`make check-needed`).

### 13.7 challoc extensions

An export that no other allocator has exists only for a consumer's
measured cost. `challoc_version()` and `challoc_features()` (section 16)
and four size-returning entry points, declared in `<challoc/challoc.h>`
(and by the compat `<jemalloc/jemalloc.h>`, which defines
`JEMALLOC_CHALLOC_USABLE` for compile-time tests), are the whole
challoc-specific surface:

```c
void  *challoc_malloc_usable(size_t size, size_t *usable);
void  *challoc_calloc_usable(size_t n, size_t size, size_t *usable);
void  *challoc_realloc_usable(void *p, size_t size, size_t *usable);
size_t challoc_free_usable(void *p);   /* frees p, returns its usable size */
```

They exist for Valkey, whose `zmalloc.c` calls `je_malloc_usable_size`
after every allocation and before every unsized free for its
`used_memory` accounting. That query classifies the pointer a second time,
and its exact liveness test loads a bitmap word nothing has touched since
the refill, an L1 miss per allocation. Rather than drop the liveness test,
these remove the second lookup: the allocation knows its class from the
pop, the free from its own classification.

Each is the standard function of the same name, then the usable size,
exactly what `malloc_usable_size` would return at that moment, stored
through `usable` (which may be null) or returned. `errno`, null handling,
alignment and the aborts are the standard functions': a failed
allocation returns null with `*usable = 0`; `challoc_free_usable(NULL)` is
0; `challoc_realloc_usable(p, 0)` frees and reports 0.
`challoc_calloc_usable` zeroes the whole usable size, where plain `calloc`
clears the requested bytes; `challoc_free_usable` is the exact `free` of
6.3 (bit test, owner word), not the sized path. In `api/libc.rs`
they are `malloc`'s pop and `free`'s check and push plus the class size:
`challoc_free_usable` calls `free_usable`, whose fast path is `free`'s
`heap::free_fast_slot` followed by `class_size(class)`; the other entry
points' machine code is unchanged by them (`scripts/codegen-diff.sh`).
Valkey uses them under `USE_CHALLOC`
(`contrib/valkey/0002-zmalloc-use-challoc-usable-size-entry-points.patch`).

### 13.8 Stack use

The entry points work on a thread with the smallest stack glibc allows
(`PTHREAD_STACK_MIN`, 16 KiB on x86_64, of which glibc's TCB and static
TLS take about 4) and leave the caller at least 6 KiB of it, as glibc's
malloc and jemalloc do. Frames are kept small where they are, rather than
moved to another stack: the deferred cgroup discovery (section 11; the
tick of any `malloc`, `fork`'s prepare handler or a read of the figures
may run it) builds its record in place in its cell
(`Deferred::cell_discover`) with its scan and mount buffers in a static
the discovering thread owns (`sys::cgroup::discovery_scratch`), and an
arena's build writes its statistics record in place (`Arena::put_stats`),
where through a value each went by the stack of whichever thread did the
work. The statistics readers and reports (`epoch`, `mallctl`'s
`stats.arenas.*`, `malloc_stats_print`, `malloc_info`, `malloc_stats`,
`mallinfo`) collect the per-arena record (`stats::ArenaStats`, about
6 KiB with its bins) into a scratch object of two such records
(`stats::Scratch`), which the entry point takes before any lock and
gives back before it returns, never holding a lock across a report's
callback, which runs on the caller's stack and may itself allocate or
report. The objects come only from a pool of 64 metadata objects, kept
once allocated: a call whose callback never returns (`longjmp`,
`pthread_exit`) abandons its object to the pool, and the thread's next
call from as high on its stack (every such object it finds, keeping one),
or any call once the thread has exited, takes it back. A slot's stack
mark moves by compare-exchange and a release frees the mark before the
owner, so a call interrupted by a signal handler that reports on the same
stack never takes the object the handler used and gave back. A `fork` from
inside a report's callback: the child's fork handler re-owns the forking
thread's objects under its new thread ID, so the child's copy of the
report keeps its object rather than having it taken as an exited
thread's. With no object free or reclaimable (64 reports held at
once, or abandoned and not yet reclaimed), or no metadata to allocate
one, `mallctl` returns `EAGAIN`, `mallinfo` zeros and a report the line
`(no memory)` until one is given back. The pooled objects count in the
metadata figures (`stats.metadata`, and so `resident` and `mapped`;
`malloc_info`'s system and aspace sizes, `mallinfo`'s `arena`): up to
64 objects of `stats::StatsPair` (11,952 B at q16, 12,848 B at q8), each
in the smallest metadata bin that fits it (12 KiB at q16, 16 KiB at q8,
the whole bin counted in `allocated`): 768 KiB at q16 and 1 MiB at q8 at
most, and as many as the most concurrent reports
the process has made. A text
`malloc_stats_print` leaves its caller about 1 KiB less of the stack than
`malloc` does (its sink's 1 KiB buffer; on x86_64 it runs with 7.5 KiB of
a `PTHREAD_STACK_MIN` thread already used, `malloc` with 8.5); glibc's
`malloc_info` needs more than either. Init is the exception the frame
budget caps rather than bounds: challoc initialises before `main` (ld.so's
and libc's first allocations), not on a program's small thread; its frame
is about 7 KiB (`/etc/challoc.conf` is read into a static,
`config::conf_buffer`), and under `thp:always` it also runs the by-value
discovery (8.6 KiB), about 16 KiB in all.

`tests/c/small_stack.c` runs the entry points on a `PTHREAD_STACK_MIN`
thread with 6 KiB of it already used, each in a fresh process (the
first allocation of `main`, a thread's first allocation building its
arena, the discovery under the tick and under `fork`, a `fork` from an
`SA_ONSTACK` handler, every statistics entry point, alone and from eight
such threads at once). The `api::` test `every_frame_fits_the_budget`
fails on any function of the shipped library that reserves more than
2 KiB of stack and is not one of a few known frames on cold paths
within a cap.

## 14. Hardening summary

[hardening.md](hardening.md) compares these properties with jemalloc's and
lists the windows the defaults leave.

Always on:

- Out-of-band metadata; no allocator pointers or sizes inside user
  memory.
- Size-class isolation in separate randomised regions with unmapped gaps.
- Invalid-free and stale double-free detection on every plain `free`
  for slots no thread cache holds (a slot a refill claimed and `malloc`
  has not handed out passes the bit test; the sized path defers the bit
  test to the flush, 6.3, unless `exact_sized_free` is set). A second free
  of a slot still held in a thread cache is caught only by a flush that
  reaches the copy while the slot is unclaimed (6.3).
- A guard after every huge run (`MADV_GUARD_INSTALL`, else `PROT_NONE`;
  none under `guards:false`).
- Abort, never continue, on any inconsistency: a message on stderr via
  `write(2)`, then `abort()` (`sys/abort.rs`). A resource failure is not
  an inconsistency (14.1).

Kernel-gated, otherwise absent: guard slabs every `guard_slab_interval` x
64 KiB of slabs (`MADV_GUARD_INSTALL`, 6.13+; section 5); sealing of the
never-committed parts of the reservations (`mseal`, 6.10+, unless seccomp
filters it; sections 2 and 10).

Off by default, tunable (each is over the budget a default-on hardening
must meet; the prices are in 6.3 and [tuning.md](tuning.md)):

- `double_free_scan`: a state byte per slot, checked and moved by every
  free route, so a double or invalid free aborts at the call (6.3).
- `exact_sized_free`: sized frees run plain `free`'s allocation check, so
  every sized free takes the slow path (6.3); its price is not measured.
- `guard_slab_interval` below the default of 8, for denser guard slabs.
- `zero_on_free`: clear a slot's class-size bytes, or a huge run's usable
  bytes, on free (6.3).
- `slot_randomize`: claim from a random bitmap word and shuffle each
  refill batch (section 4).
- `pac_pointers` (arm64 with PAC; inert elsewhere): sign every cached slot
  pointer and the TLS heap pointer (6.1, 6.2), so a write into the
  metadata region cannot make `malloc` return a chosen address. It costs
  about 11% of `xmalloc-test`'s cross-thread frees and 1..2% of `larson`.

Planned, not implemented: tagged pointers (section 18). Not
attempted: quarantine (defeats defragmentation and costs throughput) and
canaries (need bytes inside the block). Sampled guarded allocations in the
style of GWP-ASan and type-isolated heaps for Clang's
`-fsanitize=alloc-token` are roadmap items.

### 14.1 Resource failures

Every abort is one of two kinds, and a third kind of failure never
aborts:

- **Misuse or heap inconsistency**: an invalid, double or mis-sized free,
  a guard-slab address, an owner word or descriptor naming no arena, a
  state byte or count no valid history produces, a purge `madvise` that
  says our own reservation is gone or was sealed by the program
  (`ENOMEM`, `EPERM`), an invalid `MALLOCX_TCACHE`, an allocation during
  init. Continuing would hand out memory twice or into someone else's;
  these abort with a `challoc:` line.
- **Init below the documented minimum**: the address space for the
  minimum layout (section 2), `getrandom`, the fork handlers, or the
  metadata init commits (`cannot set up the allocator tables ...`, a fraction of
  a MiB once eager carving is off). Nothing has been handed out yet and
  the process cannot run under any allocator design that needs these.
- **Resource failure after init** (the kernel or a limit refuses
  memory, address space, a mapping or a guard; the metadata region is
  full): never an abort. An allocation returns NULL with `errno` =
  `ENOMEM` (C++ `operator new` throws), keeps no record of the failure,
  and the next call tries again, so when the pressure lifts (a raised
  `RLIMIT_DATA`, `munlockall`, mappings unmapped) allocation works
  again. Inside `free`, which cannot fail, the memory the failure leaves
  in doubt is kept out of circulation (leaked and counted), never
  recycled and never an abort.

Thread cancellation is not a failure either: challoc never acts on a
(deferred) cancellation request (8.1), so none leaves its state half set.
A report's call-outs to the program's callback run with cancellation
disabled, as `write_file`'s `fwrite` does (8.1); a callback that leaves
the call by `pthread_exit` or `longjmp`, or re-enables cancellation,
breaks premise C3. The exception is `operator new`'s call into the
program's `new_handler`, which runs with cancellation enabled, so a
cancel may be acted on inside it (8.1); a `new_handler` that leaves the
call by `pthread_exit` or `longjmp` breaks C4, whatever the cancel
state. `operator new` holds no lock and no half-set state of challoc's
while its handler runs: the failed attempt has returned.

Guards degrade, never vanish: a guard the kernel refuses as a
`MADV_GUARD_INSTALL` marker (on an `mlock`ed range, `EINVAL`) is a
`PROT_NONE` page or slab instead, put in place before the memory next to
it is handed out; if that `mprotect` fails too (a VMA split refused at
`vm.max_map_count`) nothing next to it is handed out and the allocation
is NULL/`ENOMEM`, retried on the next call (`SlabTable::install_guard`,
`Huge::prepare`). Inside `free`, the huge descriptor table never needs
to grow: every allocation reserves one key per live run for the frees
(`Huge::alloc_aligned`), so a free after the metadata region filled
still finds room, and `MADV_GUARD_REMOVE` fails only on a range the
program unmapped or sealed, which is an inconsistency like the purge's
`ENOMEM`/`EPERM`.

Mappings (`vm.max_map_count`, 65530 by default) are a resource too, and
the VMAs challoc creates are bounded by what is live or by a fixed or
peak quantity, never by the history of allocations: the three
reservations and the sealed gaps (fixed at init, section 2); one VMA per
committed metadata table prefix (a fixed set of tables, section 2); a
class region's committed slabs, one VMA unless a guard marker was
refused (then at most two per guard interval of the region's peak,
section 5); the huge region's committed pages, one VMA plus at most two
per live guarded run in the `PROT_NONE` fallback (none in guard-install
mode or under `guards:false`; released at free, section 7; in a `fork`
child, at most two per run live at the fork may stay, a kernel
`anon_vma` matter; the fallback keeps each new run's first page
resident to make the merge possible). Under
`thp:always` a huge run of a PMD or more is `MADV_HUGEPAGE`d, a VMA flag
the range keeps after the free, so the boundaries between advised and
unadvised ranges are bounded by the huge region's high-water mark over
the PMD size rather than by the live runs (`thp` defaults to `never`,
which advises no huge run). The class regions' `thp:always` chunk advice
covers whole committed chunks, so it merges as the region grows.

The limits a process may run under are part of this. A process that
starts under a finite `RLIMIT_DATA` has no table writable before it is
used (section 2: the policy is probed once, at the first table, so a
limit set later finds the eager tables already counted). An `RLIMIT_AS`
that covers the minimum layout on top of the program's own mappings
(the binary, ld.so, libc, the stack: 436 GiB at quantum 16, 468 GiB at
quantum 8) always lets init reserve.

## 15. Per-CPU mode

Not implemented. Per-CPU caches indexed by the rseq `mm_cid`, with the
pop and push as rseq critical sections, were prototyped and removed: the
critical sections cost 33..47 instructions per operation, which made
every reference row slower than thread caches (`larson10` 9..17%,
`xmalloc-test` 16..19%), and the mode won RSS only when threads far
outnumber CPUs. The code is in the tag `archive/prototype-2026-09-30`.
`percpu_arena` is
accepted for jemalloc compatibility and has no effect (section 12): caches
are per thread whatever its value.

## 16. Observability

- `malloc_stats_print` (`api/print.rs`) emits a report in jemalloc's
  shape, banner included, so consumers that key on it keep working:
  totals, per-arena blocks, a per-class table (`size nregs curslabs
  curregs`, the compaction counters of 6.4) and a per-class occupancy
  histogram of the non-empty slabs (slabs per eighth of `nslots`), so a
  soak's `MEMORY MALLOC-STATS` shows where the live data sits. It also
  prints a `cgroup:` line (the directory, effective `limit` and `high`,
  the last `current` sample, the `memory.events` counters; or `cgroup:
  not tracked (<reason>)`), a `cgroup policy:` line (the band, arena 0's
  effective `decay_ms`, `retention_cap`, `empty_bytes`, `full_purges`,
  whether `thp` was forced to `never`), a `purge advice:` line (the next
  purge's advice and why, batches and bytes per advice, whether `Purged`
  memory is still known zero, the ranges and bytes whose discard failed)
  and a `thp:` line (section 17.1: key, mode in force and why, the ranges
  advised, the host's global mode). The JSON form (`"J"`) has matching
  `"cgroup"` and `"thp"` objects. The options list marks inert keys
  "accepted, no effect". The line heads are pinned
  by `print::tests::text_report_line_set_is_pinned` and the JSON shape by
  `json_report_shape_is_pinned`.
- Mappings are named `challoc:classes`, `challoc:huge` and `challoc:meta`
  (`PR_SET_VMA_ANON_NAME`, when the kernel has it), so `Rss` per name in
  `/proc/<pid>/smaps` places memory by region.
- Abort messages include the faulting pointer, its classification and the
  reason, prefixed `challoc: `. `stats_print:true` prints nothing at exit.
- `challoc_version()` (`const char *`, the crate version) is the `dlsym`
  probe for "is challoc the loaded allocator"; it touches no allocator
  state and does not initialise the allocator. `mallctl("version")` stays
  the jemalloc-shaped `5.3.0-0-gchalloc`.
- `challoc_features()` (`const char *`) is the feature report: one line of
  `key=value` pairs (`sys/features.rs`, `Features::words`), the kernel
  probes, then glibc's rseq registration, then the CPU:

  ```
  page_size=4096 guard_install=yes mseal=yes anon_name=yes thp=madvise mthp=2M rseq=yes rseq_size=33 pac=no
  ```

  `thp` is the host's THP setting or `none`, `mthp` the folio sizes a
  `MADV_HUGEPAGE`d range can get or `none`. The string is built once at
  init without allocating and is byte-identical to the `Kernel and CPU
  features:` line of `malloc_stats_print`; calling it initialises the
  allocator. Bits are `yes`/`no`, keys are never renamed, and a key goes
  only with the feature it reports. `tests/c/features.c` lists the same
  keys.

## 17. Floor and ceiling

The rule is stated in section 1: the floor (glibc 2.44, kernel 5.14,
Wolfi's compiler baseline) is a compatibility promise; the ceiling is
every feature the host offers, found at run time. This section is the
inventory: every feature the allocator probes, who provides it, what the
allocator turns on with it, and what it does without it. Nothing in this
table is decided at build time.

| Feature | Provided by | challoc turns on | Fallback |
|---|---|---|---|
| Page size (`AT_PAGESZ`) | kernel, the auxiliary vector | the granularity of every mapping, slab and purge (4, 16 or 64 KiB; page-sized slabs, section 3) | none: init aborts without it |
| `MADV_GUARD_INSTALL` | kernel 6.13 | guard slabs between slabs and guards after huge runs (sections 5, 7) | a `PROT_NONE` page after huge runs; no guard slabs |
| `MADV_FREE` | every supported kernel (4.5+) | lazy purges under 70% of a known cgroup limit (sections 8, 11) | not needed; `MADV_DONTNEED` everywhere else |
| `mseal` | kernel 6.10, not filtered by seccomp | seals the never-committed ranges of the reservations at init (section 10 step 5) | skip; `Sealed ranges: 0 of N` |
| `PR_SET_VMA_ANON_NAME` | kernel 5.17 with `CONFIG_ANON_VMA_NAME` | named `challoc:*` mappings in `/proc/pid/maps` | skip |
| rseq registration | glibc 2.44's registered area (`__rseq_size`, `__rseq_offset`, `__rseq_flags`, and the area's own `cpu_id`; `rseq::probe`) | the feature line's `rseq` and `rseq_size` words, nothing else | `rseq=no rseq_size=0` without a registration (`glibc.pthread.rseq=0`, a kernel without `CONFIG_RSEQ`) |
| Transparent huge pages | any kernel with `CONFIG_TRANSPARENT_HUGEPAGE`; per-size `hugepages-<n>kB` knobs since 6.8 | opt-in through the `thp` key (17.1); the default `never` keeps the slab and metadata regions off huge pages | small pages; `thp=none mthp=none` on a kernel without THP |
| PAC (`HWCAP_PACA` and `HWCAP_PACG`) | arm64 CPU (Armv8.3) and kernel | signed cache entries and TLS heap pointer behind `pac_pointers:true` (sections 6.1, 6.2, 14); FEAT_FPAC decides whether a failure faults at the `autda` or on first use | plain pointers, the same binary, one predicted branch on a byte the fast paths load anyway |
| Model-checked protocol (`cargo test model::`, `src/model/`) | the unit test suite, no hardware: an exhaustive interleaving model of the claim, release, `on_live_zero` and purge steps of `class/`, memoised on the whole state, with the invariants of sections 4 and 5 checked at every state (no slot handed to two claimers, a slot reported zero reads zero, `live` equals set bits plus in-flight reservations, a slab in an arena's set is `Empty`/`Purged` with `live == 0` and owned by that arena, a `Purged` slab reads zero) | nothing at run time: it is the proof obligation for a change to the protocol; replaying the pre-fix step order finds the `calloc` race of section 5 | none needed |

Below the floor nothing is promised, but kernel 5.10 is exercised in CI
as a tripwire: every row above degrades to its fallback there. (The
`tripwire` check is off until its Debian 11 bench image is rebuilt,
#463; until then nothing checks 5.10.)

### 17.1 Transparent huge pages

The `thp` key (`thp.rs`) against the host's sysfs table
(`sys/thp_host.rs`):

- `never` (the default, alias `default`): one `MADV_NOHUGEPAGE` each on
  the class superblock and the metadata reservation
  (`thp::advise_reservation` from `Regions::reserve`, again from
  `thp::force_never`), a VMA flag every later split inherits, so on a host
  whose global mode is `always` (Chainguard OS ships `always`) no slab or
  table of challoc's gets a huge page; the fast paths are byte for byte
  those of the allocator without the key. The huge region is not advised
  and follows the host's policy: a huge run is a whole allocation the
  program asked for, as jemalloc's large extents are (advising it too cost
  the Valkey model about 10% on Chainguard OS for no RSS gain).
  `tests/c/thp.c never` checks both halves.
- `auto`: `MADV_HUGEPAGE` on the slab groups (at `take_group`) of every
  class whose slab holds an enabled folio size below the PMD (64 KiB
  slabs and up with `hugepages-64kB` enabled), and metadata commits
  rounded to that folio; never a 2 MiB page, so the fork copy-on-write
  cost that makes Redis and Valkey forbid huge pages stays absent. Inert,
  with the reason in the report, where no such size is enabled or where
  the PMD size is `madvise`-gated (the advice would then make the regions
  eligible for 2 MiB pages). Not the default: it costs `larson` about 1.6%
  and 7..19% more RSS on small heaps for a gain only on dTLB-bound rows.
- `always`: with the PMD size enabled and no larger than 2 MiB
  (`thp::LG_PMD_CHUNK_MAX`; at 16 and 64 KiB pages the PMD is 32 and
  512 MiB, and a chunk per class would be the RSS floor), class regions
  are committed and advised in PMD-sized chunks (`commit_slab`), huge
  runs of a PMD or more are advised (section 7), and the metadata takes
  the sub-PMD folio if one is enabled. A guard slab inside a chunk is left
  unused but unguarded, since a guard would split the 2 MiB page; a purge
  inside the page splits it anyway, the kernel's cost of the mode. On a
  larger PMD `always` is the folio path, inert (`PmdTooLarge`) when no
  folio size is enabled. Forced to `never` under a cgroup limit below
  1 GiB (section 11). It buys the Valkey model's hot `SET` loop 5..18% for
  about 10% more peak RSS on that heap and 30..66% more on allocation
  microbenchmarks, hence an opt-in.

`auto` and `always` issue no `MADV_NOHUGEPAGE`, so on an `always` host
their unadvised regions stay eligible for the host's PMD pages. The
`thp:` report line and `stats.thp.*` (13.4) say which mode is in force,
why, and how many ranges each path advised.

### 17.2 Time slice extension

Not implemented. The rseq time slice extension (kernel 7.0,
`CONFIG_RSEQ_SLICE_EXTENSION`) was prototyped for lock holders and
measured on the one reference kernel that had it: no row moved beyond
the noise margin, so it was removed (tag
`archive/prototype-2026-09-30`).

## 18. Tagged pointers

Designed, not implemented. No
code path exists, no `tag_pointers` key is parsed, and nothing here
changes a default. The full design is section 18 of `docs/design.md` at
the tag `archive/prototype-2026-09-30`; this is its summary, corrected
where the hardware outlook has changed.

### 18.1 Hardware

MTE (Armv8.5 `FEAT_MTE`) gives every 16-byte granule a 4-bit allocation
tag checked by the hardware on every access; of the hosts in reach only
AmpereOne bare metal exposes it to Linux user space (Neoverse V2 parts
such as Axion and Graviton4 do not). Intel LAM is compile-time disabled
in current Linux; AMD UAI has no Linux enable path (a 2022 RFC was never
merged) and is not a target. TBI (top byte ignore) is on every arm64
host, so a software-checked tag needs no special hardware there.

### 18.2 MTE

Commits pass `PROT_MTE`; tag 0 is excluded from generated tags, so an
untagged or forged pointer faults. A cache holds only pointers whose tag
equals their slot's allocation tag, so the `malloc` pop is untouched.
`free` checks the pointer's tag against the slot's (`ldg`), then retags
the slot (`irg`, `st2g`/`dc gva`) before the push, so every pointer the
program still holds is stale in hardware and a read through it faults.
The expected cost is `zero_on_free`'s store pattern plus a tag load per
free, far outside a default-on budget: an opt-in per consumer.

### 18.3 The software tag (TBI, LAM)

Where the hardware ignores the top bits but does not check them, the
allocator carries a 4-bit tag there and verifies it itself where the
pointer comes back (`free`, `realloc`, `malloc_usable_size`, the sized
path); loads and stores are not checked. The tag is `gen(slot) ^
key[class]`: a per-slot generation in a byte map beside the slot bitmap
and a per-class key from `getrandom`; `free` compares, increments the
generation and pushes the retagged pointer. Four bits is the width
everywhere: the "Simplifying LAM" kernel series cuts the LAM user tag
from 6 bits to 4, the width of MTE and of the planned x86 ChkTag ISA, so
one format serves LAM (if it returns), MTE and TBI. TBI has 8 bits and
could carry up to 6, but the design keeps 4 there too, so the aarch64
software mode is the same code as LAM with a different mask. A stale,
forged or cross-class pointer passes with probability 1/16.

### 18.4 Stripping and configuration

Every entry point that takes a pointer would strip the tag first (`a = p
& addr_mask`, a word on the `Shared` line the fast paths already load,
`!0` with tagging off): one `and` per `free`-family call in the default
configuration, to be priced before it lands. Planned keys: `tag_pointers`
(`off`, `mte`, `lam`, `auto`; default `off`) and `tag_check` (`async`,
`asymm`, `sync`; MTE only), read back as `opt.` strings and reported as a
`Tagged pointers:` line; `challoc_features()` would gain hardware words,
not the effective mode.

## 19. Source layout

```
src/lib.rs            crate root, quantum feature selection, panic handler
src/testutil.rs       test-only fixtures: TestShared (a Shared over a synthetic layout), xorshift
src/sys/mod.rs        kernel constants; typed syscall wrappers (mmap, mprotect, madvise, mseal, prctl, getrandom, futex, clock_gettime, open/read/close, write, signals); read_small_file
src/sys/abort.rs      the abort path: `challoc: <msg>` on stderr, SIGABRT the way glibc delivers it, the `abort!` macro
src/sys/auxv.rs       getauxval and the AT_* keys (AT_PAGESZ, AT_HWCAP, AT_SECURE)
src/sys/raw.rs        the asm syscall instructions (x86_64, aarch64)
src/sys/nr.rs         syscall numbers, header-checked (tests/c/sysnr.c)
src/sys/errno.rs      kernel error decoding
src/sys/features.rs   kernel feature probing; the feature line as a key table (words)
src/sys/thp_host.rs   the THP sysfs table (enabled, hpage_pmd_size, hugepages-<n>kB) and its thp=/mthp= words
src/sys/cgroup.rs     cgroup2 discovery, limit walk, sampling (memory.current, memory.events)
src/sys/pac.rs        pointer authentication (arm64 FEAT_PAuth): probe, sign, authenticate
src/sys/tls.rs        the initial-exec TLS accessor (6.1)
src/rseq.rs           glibc's rseq registration, read for the feature line's rseq= words
src/lock.rs           futex lock, padded lock
src/lock/tests.rs     lock tests
src/sync.rs           the atomics the vspec wrappers use: core's, or loom's under cfg(loom)
src/layout/mod.rs     size classes (Class, CLASSES, class_of), slab geometry, adopt_page_geometry
src/layout/regions.rs Regions: the reservations, pointer classification, seal; the section 2 parameters
src/layout/tests.rs, src/layout/linux_tests.rs   layout tests
src/bitmap.rs         atomic bitmap and TwoLevelBitmap
src/slab.rs           SlabDesc (the descriptor only)
src/class/mod.rs      ClassState: claim, release, reopen, holes, stats; the test-only protocol step enums the model matches
src/class/table.rs    SlabTable: descriptors, slot bitmap, owner array, guard slabs, take_group, commit_run, commit_slab
src/class/sets.rs     Lazy (a lazily committed metadata table), Set, SetTable
src/class/fresh.rs    the fresh-slab path: next slab of the arena's group, next group
src/class/complete.rs ghost model of one class's slab state per arena: a refill's claim never misses free capacity
src/class/decay.rs    purge_empty and the decay walk (section 8)
src/class/verify.rs   the check_zero debug mode and the test-only pointer checks
src/class/abort.rs    the #[cold] heap-inconsistency aborts of the class protocol
src/class/tests.rs    class tests over a synthetic region (Env)
src/heap/mod.rs       Heap and its constants (MAX_CAP, MIN_CAP, FOREIGN, PUSH_*); the crate-facing entry functions
src/heap/cache.rs     CacheHeader: pop and push primitives, first-use setup (ensure_cache)
src/heap/paths.rs     the inlined fast paths, the cold slow paths, free_other
src/heap/fresh.rs     a fresh heap's plain fields and the FreshHeap token that hands on their permission
src/heap/live.rs      the live-block ghost model: a Live token per handed-out slot or huge run
src/heap/zero.rs      the zero prefix of the thread caches as ghost tokens (no zeroing entry point trusts it)
src/heap/flush.rs     refill and flush; group_by_slab and the straddling double-free abort
src/heap/compact.rs   the compacting cache: probe/compact modes, scheduled pop, rebuild (6.4)
src/heap/others.rs    caches for other arenas (ArenaCaches) and the FOREIGN flush-only cache
src/heap/theorems.rs  the block and region theorems (separation, guards, overflow reach, metadata integrity)
src/heap/perms.rs     the CachePerms lemmas: the ghost model of the caches' stacks and slot tokens
src/heap/shared.rs    Shared: process-wide state, init, the free checks and routing, the class lines, the thread-exit key
src/heap/thread.rs    thread lifecycle (6.5): TLS sentinels, heap creation, the exit destructor
src/heap/tcaches.rs   explicit caches (tcache.create)
src/heap/slotstate.rs the per-slot state bytes (free, cached, live) of the double-free scan (6.3)
src/heap/heap_facts.rs  the set-free checker of what tcaches::heap_at trusts, shared by heap_at_tests and tlscheck
src/heap/tlscheck.rs  debug-only (cfg(challoc_tls_check)) run-time check of the TLS slot premise
src/heap/mallocx.rs   MALLOCX_* flag decoding and the mallocx/dallocx routing
src/heap/tests.rs, src/heap/linux_tests.rs   heap tests
src/heap/heap_at_tests.rs, src/api/heap_at_tests.rs   test-only: the heap_at checker on every heap heap_at hands out, and through the C API's MALLOCX_TCACHE paths
src/huge/mod.rs       huge allocator: extents, guards, first fit, the audit
src/huge/decay.rs     the huge purge and decay walks (purge_empty, decay_with) and their Tally
src/huge/table.rs     the open-addressing descriptor table (page number to HugeDesc)
src/huge/tests.rs, src/huge/linux_tests.rs   huge tests
src/meta.rs           metadata region allocator (objects and tables, eager carving)
src/model/            test-only: the exhaustive interleaving model of the slab protocol (mod, state, steps, scenarios; section 17)
src/purge.rs          decay driver, Batch and advice, Decayer, purged_is_zero, on_tick
src/purge/curve.rs    the decay period and curve (NEVER, STEPS, WEIGHTS, Curve)
src/purge/background.rs   the background thread (8.1)
src/purge/tests.rs, src/purge/linux_tests.rs   purge tests
src/policy.rs         the cgroup policy (section 11): bands, retention cap, full purge, Advice
src/arena.rs          arenas and the arena registry
src/config.rs         malloc_conf parsing: one key table (TABLE) behind set, get and KEYS
src/stats.rs          counters and the epoch merge
src/stats/ledger.rs   ghost model of what the exact-stats counters count and where those blocks are
src/thp.rs            the thp key against the sysfs table: reservation advice, folios, chunks, huge runs, force_never
src/api/mod.rs        lazy init, the Shared/Features statics, challoc_version/challoc_features, fork handlers
src/api/libc.rs       standard entry points, malloc_trim, the challoc_*_usable entry points
src/api/glibc.rs      mallopt, mallinfo, mallinfo2
src/api/jemalloc.rs   *allocx, mallctl entry points, malloc_conf/malloc_message symbols, get_defrag_hint, je_ aliases
src/api/cxx.rs        operator new / delete
src/api/mallctl.rs    the control namespace
src/api/print.rs      malloc_stats_print, malloc_stats, malloc_info
src/api/tests.rs, src/api/mallctl/tests.rs   api and mallctl unit tests
src/api/c_programs.rs test-only: the C-test table and its driver (tests/c, fuzz/, tests/glibc, tests/mimalloc)
src/api/restated.rs   test-only: checks the restating theorems still make their bodies' calls (verus/README.md)
src/vspec/            Verus seams: contracts for core functions vstd does not specify, trusted wrappers
src/kani/             Kani harnesses for seams of the ledger (cfg(kani) only)
include/              the installed headers
tests/c/              C programs run under LD_PRELOAD against the cdylib (common.h is the shared harness; scripts/ctest-cc.sh the one compiler line)
tests/glibc/, tests/mimalloc/  vendored suites
tests/loom/           loom models of the atomics protocols (its own crate; tests/pins.rs keeps them tied to the source)
tests/consumers/      Valkey and Ruby builds on challoc
fuzz/                 randomized alloc/free driver with seeds
bench/                benchmark drivers and probes (bench/README.md)
contrib/              the Valkey patches and the Wolfi package definitions
scripts/              dev scripts; codegen-diff.sh proves a refactor left the machine code unchanged
ci/                   the scripts the Chainguard Checks (.chainguard/ci/) call (ci/README.md)
verus/                Verus tooling: coverage counter and FLOOR, seam ledger (verus/README.md, verus/SEAMS.md)
docs/                 design, tuning, hardening, testing, benchmarking, research
```

Pure logic (`layout`, `bitmap`, size-class math, config parsing, and the
cgroup file parsing and limit walk) is unit tested against fixtures and
captured inputs, with no dependence on the kernel the tests run on. Every
test, pure or not, runs with `cargo test` on Linux; the crate links
`std` only under `cfg(test)`.

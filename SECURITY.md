# Security model

challoc is a `malloc` replacement. Every allocation and free of the
program that loads it goes through it, so its job is to keep a bug in
that program from turning into control of the heap. This file says
what challoc defends against, what it guarantees, what is best-effort,
and what is out of scope. `docs/hardening.md` has the exact windows and
compares each property with jemalloc. `docs/design.md` section 14 is
the design view.

To report a vulnerability, follow the
[Wolfi security policy](https://github.com/wolfi-dev/.github/blob/main/SECURITY.md).
Don't open a public issue. Include the commit, the architecture, the
kernel version and the `challoc_features()` line (or the `Kernel and
CPU features:` line of `malloc_stats_print`). challoc has no releases
yet, so fixes go to `main`.

## Attacker model

The attacker is a heap-corruption bug in the host program, driven by
input the attacker controls:

- a double free, or a free of a pointer challoc never returned (an
  interior, stack, global or foreign-heap pointer);
- a sized free (`sdallocx`, `free_sized`, `free_aligned_sized`, sized
  `operator delete`) whose size doesn't match the block;
- a linear overflow or underflow from a block into its neighbours;
- a use after free: reads and writes through a stale pointer.

The goal is to stop these bugs from becoming the usual allocator
exploits: `malloc` returning memory that overlaps a live block or the
allocator's metadata, `malloc` returning an address the attacker chose,
or freed data leaking to the next owner. A bug that gives the attacker
an arbitrary write anywhere in the address space is beyond what an
allocator can contain (see "Out of scope").

## Trust boundaries

- **Metadata is out of band.** Slab descriptors, slot bitmaps, thread
  caches and heaps live in a separate metadata region, never in user
  memory. Freed blocks hold no free-list pointers or sizes. An overflow
  from a block reaches other blocks of the same size class, not
  allocator state.
- **The size class comes from the address.** challoc finds a pointer's
  class from the address range it falls in, not from a header the
  program can overwrite.
- **Configuration is trusted.** The `malloc_conf` and `je_malloc_conf`
  globals, `/etc/challoc.conf`, `MALLOC_CONF`, `CHALLOC_CONF` and
  `mallctl` writes come from the program, its packager or its operator.
  They can turn hardening off or degrade performance, and that is not a
  vulnerability. The one exception is privilege: when the process is
  `AT_SECURE` (setuid, setgid or capability-elevated), challoc ignores
  `MALLOC_CONF` and `CHALLOC_CONF`, following glibc's `secure_getenv`
  rule. A configuration value that crashes challoc or corrupts its
  state, as opposed to choosing a weaker setting, is a bug in scope.

## What challoc guarantees

These hold on every supported host with the default configuration:

- **Invalid free aborts.** A plain `free` of a pointer challoc didn't
  return aborts at the call, unless it is a slot start some thread cache
  holds (see "Frees of slots a refill claimed" below).
- **Stale double free aborts.** A second `free` of a slot whose slot
  bit is clear (released to its slab and not handed out again) aborts
  at the call.
- **Mis-sized free aborts.** A sized free whose size names another
  class (for a huge run, another page count) aborts at the call.
- **Foreign pointers abort** in `malloc_usable_size`, `realloc` and
  `sallocx`, where jemalloc's behaviour is undefined. So does a freed
  pointer whose slot bit is clear (released to its slab and not handed
  out again).
- **Abort, never continue.** On any inconsistency challoc writes a
  `challoc:` message to stderr and calls `abort()`. It never tries to
  repair the heap.
- **Classes are isolated.** Each size class lives in its own region at
  a randomised offset, with never-committed `PROT_NONE` gaps between
  regions, so an overflow can't walk from one class into another.
- **Huge runs are guarded.** Every huge run is followed by a guard page
  (`MADV_GUARD_INSTALL`, or a `PROT_NONE` page on older kernels).
- **Region edges are guarded.** Every class region, the huge region and
  the metadata region has at least 4 MiB of never-committed reservation
  directly below and above it, whatever the random offsets, so a linear
  overflow or underflow off a region's edge faults before it can reach
  another mapping.
- **Zeroing allocations are zero.** `calloc`, `challoc_calloc_usable`
  and `MALLOCX_ZERO` (in `mallocx` and `rallocx`) return zeroed bytes
  even when an overflow, an underflow or a write through a stale pointer
  has already written memory challoc has not handed out yet. challoc
  clears every slab slot itself. A huge run is trusted zero only for
  pages no run has ever covered (past the huge region's high-water mark,
  behind the last run's guard) and for the pages of a purged extent it
  discards again with `MADV_DONTNEED` as it hands the run out; anything
  else is cleared.

## Best-effort and probabilistic protections

- **Frees of slots a refill claimed.** A refill claims a batch of slots
  into a thread cache and sets their bits before `malloc` hands them out.
  A plain `free` of such a slot start, one challoc has not returned yet,
  passes the bit test: it is accepted, cached, and the slot can then be
  handed out twice. `double_free_scan:true` narrows this at the call
  (it compares the slot against the four most recent entries of the
  cache the free pushes to).
- **Double frees still in a thread cache.** A second free of a slot
  still held in any thread's cache is not checked at the call. A flush
  catches it only if the slot is still unclaimed when the flush reaches
  the copy: if a `malloc` or any refill takes the slot
  first, it is live twice. `double_free_scan:true` narrows this window
  at the call, at a measured cost.
- **Freed pointers still in the thread cache.** A slot keeps its live
  bit until a flush releases it, so `malloc_usable_size`, `realloc` or
  `sallocx` on a pointer freed into the cache (and not yet flushed) is
  not detected: it reports the class's usable size, or reallocates.
- **The sized path defers its slot check.** `sdallocx` and `free_sized`
  check the size at the call but leave the invalid- and double-free
  check to a flush, with no bound on when. A sized free of any slot start
  of the named class, including one challoc never returned, enters the
  thread cache: the thread's next `malloc` of that class returns it (a
  slot the caller chose, writable at once if its slab's memory is
  committed), and if a refill claims the slot before a flush, the slot is
  live twice. That deferral is jemalloc's behaviour for every free.
- **Randomisation is probabilistic.** Class region addresses are
  randomised. Slot order within a class is predictable unless
  `slot_randomize:true` is set.
- **Kernel-gated features.** Guard slabs need kernel 6.13
  (`MADV_GUARD_INSTALL`). Sealing the never-committed parts of the
  reservations needs `mseal` (kernel 6.10) and a seccomp profile that
  allows it, and Docker's default profile doesn't. Without them the
  protection is absent, but allocation results don't change.
- **Committed-ahead slabs.** An arena commits fresh slabs up to 512 KiB
  at a time. A linear overflow off the newest slab runs into committed
  but unused memory and faults only at the end of that run.
- **Opt-in hardening.** `zero_on_free` (no freed data for the next
  owner) and `pac_pointers` (arm64 with pointer authentication: signed
  cache pointers, so a write into the metadata region can't make
  `malloc` return a chosen address) are off by default. Each costs more
  than the budget for default-on hardening. `docs/tuning.md` lists the
  prices.
- **Memory safety of challoc itself.** challoc is `no_std` Rust. Part
  of it is formally verified with Verus: the `verus` check holds `main`
  to the verified share of functions and lines in `verus/FLOOR`, and
  `verus/ledger/` lists every item Verus takes on trust
  (`verus/README.md` has the current coverage). That is a reduction in
  risk, not a proof of the whole allocator.

## Out of scope

- **Use after free while the block is still allocated or reused.**
  challoc has no quarantine and no canaries. A stale pointer reads and
  writes whatever block now occupies the slot. Tagged pointers (MTE, or
  a software tag) are designed but not implemented (#50).
- **A free of a live slot the caller doesn't own.** Once a freed slot
  is released and handed out again, a stale `free` of the old pointer,
  or a forged pointer to the start of another live slot, looks exactly
  like a correct free: the slot bit is set, so it doesn't abort, and
  the next owner's block is freed under it. Tagged pointers (#50) are
  what would catch this.
- **Overflows within a size class.** An overflow into the next block
  of the same class isn't detected until it hits a guard or an
  unmapped page.
- **Arbitrary write primitives.** An attacker who can already write
  anywhere can overwrite the metadata region. `pac_pointers` raises the
  bar on arm64, and nothing else does.
- **Weak configuration.** Turning hardening off or choosing slow
  settings through a trusted configuration source.
- **Information leaks by design.** `malloc_stats_print`, `mallctl` and
  `challoc_features()` report addresses, sizes and host features to the
  program that calls them.
- **Unsupported hosts.** Anything outside the platform in `README.md`
  ("Support"): other libcs, distributions, operating systems or kernels
  below 5.14.
- **Denial of service by allocation.** A program that allocates without
  bound runs out of memory. challoc returns memory to the kernel under
  cgroup pressure, but it doesn't enforce limits.
- **Side channels** such as timing or cache contention between threads
  of the same process.

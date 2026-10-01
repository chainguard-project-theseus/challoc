# challoc overview

The goals behind challoc, why it exists next to jemalloc, and how it
works, in a few pages. `docs/design.md` is the full design, section by
section; `docs/tuning.md` the operator's guide; `docs/hardening.md` the
security properties.

## Goals

In priority order:

1. **At least as secure as jemalloc, and in practice much more so.** No
   allocator metadata lives in user-reachable memory. Every free is validated.
   Size classes are isolated in separate address ranges with guards between
   them.
2. **At least as fast as jemalloc on the workloads Wolfi actually ships.**
   Single hot thread with many small variable-size allocations and long
   uptimes, which is Redis, Valkey, and Ruby. Fewer atomics on the hot path
   than jemalloc, fragmentation behaviour designed around Valkey's active
   defragmentation.
3. **A drop-in for jemalloc.** Same symbols, same `mallctl` keys that real
   software uses, same `MALLOCX_*` flag encoding, optional `libjemalloc.so.2`
   soname. Valkey with active defrag, `MEMORY PURGE`, `MEMORY MALLOC-STATS`,
   and Lua arena isolation all keep working.
4. **cgroup aware.** challoc finds its cgroup2 at init, reads the
   effective `memory.max` and `memory.high` (the minimum up the hierarchy), samples `memory.current`
   and `memory.events` at most every 10 ms from slow paths and the
   background thread; `stats.cgroup.*` and `malloc_stats_print` expose the
   figures, and the policy acts on them: retention capped at 5% of the
   limit, the decay window shrinking towards 1 s between 70% and 90% of
   it, everything freed purged at once above 90%, a full purge when the
   kernel starts reclaiming (`memory.events` `high`), thread caches collected at every periodic tick
   above 90%, `MADV_FREE` below 70% of a known limit and `MADV_DONTNEED`
   above, and `thp:always` forced to `never` under a 1 GiB limit. The
   policy is `docs/design.md` section 11 (`src/policy.rs`), exercised by
   the container tests (`make test-cgroup`).
5. **Modern and memory safe.** `no_std` Rust, abort on panic, unsafe confined
   to a syscall layer, a thread-local accessor, and the pointer arithmetic of
   the slab allocator itself.

## Why not keep jemalloc

jemalloc was archived in June 2025 and un-archived by Meta in March 2026, with
5.3.1 following in April 2026 as the first release in four years. It is
maintained again, but by one company for one fleet, in C, with essentially no
exploit mitigations: no out-of-band metadata, no free-list hardening, no
double-free detection outside debug builds, no guard pages. Its per-CPU story
depends on `sched_getcpu`, and its memory-return policy knows nothing about the
container it is running in.

mimalloc v3 and snmalloc are excellent and fast, and mimalloc's secure mode is a
real hardening step, but neither implements the jemalloc API, so neither can
replace jemalloc in Valkey without losing defragmentation. hardened_malloc is
the security reference but is deliberately not tuned for server throughput and
has no jemalloc compatibility either.

challoc takes the fast parts of mimalloc, the hardening model of hardened_malloc,
the API of jemalloc, and adds cgroup awareness on top. See
[research.md](research.md) for the survey behind those choices.

## How it works

**Address space layout.** At first use challoc reserves a large `PROT_NONE`
range and carves it into one region per size class, each at a randomised
offset inside its own guard gap, plus a region for huge allocations and a
separate region for all allocator metadata. Finding the size class, slab, and
slot of any pointer is arithmetic on the address, and a pointer that does not
resolve to an allocated slot is rejected. Nothing about a block is stored in
or next to the block.

**Slabs and bitmaps.** Each region is divided into fixed-size slabs of one size
class, 64 KiB for small classes and larger for medium classes, except the
512..1024-byte classes, whose slabs are 4..16 KiB so that a single long-lived
value pins little; on a kernel with 16 or 64 KiB pages those classes take the
page as their slab size at init, so no slab is ever smaller than a page and
every empty slab can be returned on its own. Every slab has a
16-byte descriptor in the metadata region holding a live count, its arena,
and its decay timestamp, and one bit per slot in a dense per-class bitmap (70
KB for a 550 MiB heap of 1 KiB values, so the `free` check hits L2 instead of
missing on a descriptor line per slab). Bitmap words are updated with atomic
operations, so slabs have no owning thread and a free from any thread is
handled the same way; slabs are handed to arenas in groups that own whole
bitmap lines, so one arena's frees never read a line another arena writes.

**Thread caches.** Each thread has a heap in the metadata region, reached
through one initial-exec thread-local pointer. The heap holds a bounded LIFO
array of free slot pointers per size class. The hot path for `malloc` is a
thread-local load, a pop, and a bounds check. The hot path for `free` is a
range check, one bitmap load to confirm the slot is live, and a push. The
bitmap is only written when a cache refills from a slab or flushes to one,
which happens once per tens of operations and claims or releases up to 64
slots with a single atomic. There are no per-operation atomics and no locks on
the hot path.

**Fragmentation policy.** When a cache refills it takes the lowest-addressed
slab that has free slots, found in constant time from a two-level bitmap over
slab descriptors. That is jemalloc's address-ordered first fit, the policy
that keeps long-running heaps compact and makes Valkey's defragmentation
effective. Slab utilisation is one load away, so the defrag query that Valkey
issues is cheap.

**Huge allocations.** Requests above the largest size class are placed in the
huge region with page granularity, address-ordered, with a guard region after
each run. Freed runs are retained and decay like slabs.

**Memory return.** Empty slabs and runs are dirty and are returned in
batches of `madvise` along jemalloc's decay curve: about half
of what went empty at once within half of `dirty_decay_ms`, all of it by
the end of the window. Decay
runs from the allocating thread's slow path and, when enabled through the
jemalloc `background_thread` control, from a background thread. Under a
cgroup limit (`docs/design.md` section 11) freed-but-unpurged memory is
capped at 5% of `min(memory.high, memory.max)`, the decay window shortens
linearly from `dirty_decay_ms` at 70% of the limit to 1 s at 90%, and
above 90% the window is zero and the cache GC runs at every periodic tick;
a rise of `memory.events` `high` purges everything freed once. The advice
follows the same bands: under 70% of a known limit the return is
`MADV_FREE` (the pages stay charged until the kernel wants them, and a
reuse before that costs no page fault), everywhere else, with no limit
included, `MADV_DONTNEED` (RSS drops at once; lazily freed pages would
mislead a fragmentation ratio or an autoscaler); `lazy_band:false` makes
every return eager, and
`malloc_trim(0)` is always eager. Read `stats.cgroup.decay_ms`,
`stats.cgroup.retention_cap`, `stats.cgroup.empty_bytes` and
`stats.cgroup.purge_advice` to see the policy at work. Measured in a
`docker run --memory=512m` container (`make test-cgroup`): 400 MiB
allocated, freed and idled at 90% of the limit is back to 13 MiB of RSS
within 50 ms of the frees; with `MALLOC_CONF=cgroup:false` 319 MiB is
still resident 2 s later (jemalloc 5.3.1 keeps 190 MiB, mimalloc 3.5.2
407 MiB); 200 MiB freed at 40% of the limit is returned lazily 80 ms
later and allocating it again takes 5 ms instead of 44 (C4A); and a live
set swinging between 200 and 480 MiB runs for 5 minutes where jemalloc and
mimalloc are OOM-killed within 12 s.

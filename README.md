# challoc

A modern, memory-safe successor to jemalloc for Wolfi and Chainguard OS.

challoc is a general-purpose `malloc` written in Rust that targets glibc 2.44
and newer, uses current Linux kernel features when they are present, and falls
back cleanly when they are not. It is meant to be the allocator that Wolfi
packages such as Valkey, Ruby, KeyDB, and RethinkDB link instead of jemalloc,
so it speaks jemalloc's extended API as well as the standard C one.

Goals, in priority order:

1. **At least as secure as jemalloc, and in practice much more so.** No
   allocator metadata lives in user-reachable memory. Every free is validated.
   Size classes are isolated in separate address ranges with guards between
   them.
2. **Faster than jemalloc on the workloads Wolfi actually ships.** Single hot
   thread with many small variable-size allocations and long uptimes, which is
   Redis, Valkey, and Ruby. Fewer atomics on the hot path than jemalloc,
   fragmentation behaviour designed around Valkey's active defragmentation.
3. **A drop-in for jemalloc.** Same symbols, same `mallctl` keys that real
   software uses, same `MALLOCX_*` flag encoding, optional `libjemalloc.so.2`
   soname. Valkey with active defrag, `MEMORY PURGE`, `MEMORY MALLOC-STATS`,
   and Lua arena isolation all keep working.
4. **cgroup aware.** No allocator today reads its memory cgroup. challoc sizes
   its caches to the effective `memory.max`, returns memory eagerly as usage
   approaches `memory.high`, and can react to pressure-stall signals.
5. **Modern and memory safe.** `no_std` Rust, abort on panic, unsafe confined
   to a syscall layer, a thread-local accessor, and the pointer arithmetic of
   the slab allocator itself.

## Why not just keep jemalloc

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
[docs/research.md](docs/research.md) for the survey behind those choices.

## How it works

**Address space layout.** At first use challoc reserves a large `PROT_NONE`
range and carves it into one region per size class, each at a randomised
offset inside its own guard gap, plus a region for huge allocations and a
separate region for all allocator metadata. Finding the size class, slab, and
slot of any pointer is arithmetic on the address, and a pointer that does not
resolve to an allocated slot is rejected. Nothing about a block is stored in
or next to the block.

**Slabs and bitmaps.** Each region is divided into fixed-size slabs of one size
class, 64 KiB for small classes and larger for medium classes. Every slab has a
descriptor in the metadata region holding an allocation bitmap, a live count,
its arena, and its decay timestamp. Bitmap words are updated with atomic
operations, so slabs have no owning thread and a free from any thread is
handled the same way.

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

**Memory return.** Empty slabs and runs are dirty until a decay window passes,
then returned with `MADV_DONTNEED` in batches through `process_madvise`. Decay
runs from the allocating thread's slow path and, when enabled through the
jemalloc `background_thread` control, from a background thread. Under a cgroup
limit the decay window shortens and retention shrinks as usage approaches
`memory.high`; near the limit, purge is immediate.

**Kernel features, all runtime detected.**

| Feature                      | Kernel | Used for                                   | Fallback                          |
|------------------------------|--------|--------------------------------------------|-----------------------------------|
| `MADV_GUARD_INSTALL`         | 6.13   | Guards between slabs and after huge runs   | `PROT_NONE` page after huge runs  |
| `mseal`                      | 6.10   | Seal region table and reserved ranges      | Skip                              |
| `process_madvise` on self    | 5.10   | Batched purge                              | Loop over `madvise`               |
| `PR_SET_VMA_ANON_NAME`       | 5.17   | Named heap mappings in `/proc/pid/maps`    | Skip                              |
| PSI triggers, unprivileged   | 6.4    | Pressure-driven purge in background thread | Poll `memory.current`             |
| rseq `mm_cid`                | 6.3    | Optional per-CPU mode, later phase         | Thread caches only                |
| Transparent huge pages       | any    | Opt-in only                                | Off                               |

Chainguard OS ships Linux 6.18, so everything above except the per-CPU mode is
available there. Containers run on whatever kernel the host has, so every path
is probed at init and degrades without changing observable behaviour.

**Security properties versus jemalloc.**

| Property                                 | jemalloc 5.3 | challoc         |
|------------------------------------------|--------------|-----------------|
| Metadata separate from user memory       | No           | Yes             |
| Free-list pointers in freed blocks       | Yes          | None            |
| Invalid free detected                    | No           | Yes, aborts     |
| Double free detected                     | Debug only   | Yes, aborts (1) |
| Size classes in isolated address ranges  | No           | Yes, randomised |
| Guard regions                            | No           | Yes             |
| Zero on free                             | Opt-in       | Opt-in          |
| Slot order randomisation                 | No           | Opt-in          |
| Implementation language                  | C            | Rust            |

(1) A second free of a slot that has already left the thread cache is always
caught. A second free while the slot is still cached is caught against the
most recent entries by default and exhaustively with the `double_free_scan`
tunable.

## Compatibility

challoc exports the standard C allocation functions including C23's
`free_sized` and `free_aligned_sized`, the glibc extras such as
`malloc_usable_size`, `malloc_trim`, `mallopt`, and `mallinfo2`, and the
jemalloc surface: `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`,
`sdallocx`, `nallocx`, `mallctl`, `mallctlnametomib`, `mallctlbymib`,
`malloc_stats_print`, and the `malloc_conf` configuration string. Every
jemalloc symbol is also exported with the `je_` prefix. The build can produce
a `libjemalloc.so.2` soname for packages that only link against jemalloc.

See [docs/design.md](docs/design.md) for the full design and
[docs/plan.md](docs/plan.md) for the implementation plan.

## Status

Design stage. Nothing here is usable yet.

## Building

challoc is Linux-only and is developed inside a Wolfi container on macOS and
Linux hosts alike:

```sh
make dev                       # shell in the Wolfi dev container, repo at /src
make build                     # cargo build --release inside the container
make run CMD='cargo test'      # any one command inside the container
make test-host                 # pure-logic unit tests on the host
```

`Dockerfile.dev` defines the container. Note that a plain `apk add rust` on
`wolfi-base` currently yields a broken `rustc` because Wolfi's Rust is built
against the versioned `glibc-2.44` stream; the Dockerfile handles the swap.

Native aarch64 and x86_64 builds are both supported. Use
`--platform linux/amd64` for an emulated x86_64 build on Apple silicon when
checking portability. Emulation is not suitable for benchmarks.

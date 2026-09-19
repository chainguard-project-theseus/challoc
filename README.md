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
4. **cgroup aware.** No allocator today reads its memory cgroup. challoc
   finds its cgroup2 at init, reads the effective `memory.max` and
   `memory.high` (the minimum up the hierarchy), samples `memory.current`
   and `memory.events` at most every 10 ms from slow paths, and its
   background thread arms a PSI trigger on `memory.pressure` where the
   kernel allows it; `stats.cgroup.*` and `malloc_stats_print` expose the
   figures, and the policy acts on them: retention capped at 5% of the
   limit, the decay window shrinking towards 1 s between 70% and 90% of
   it, everything freed purged at once above 90%, a full purge when the
   kernel starts reclaiming (`memory.events` `high`), a decay pass when a
   pressure trigger fires. Sizing the caches to the limit is the next
   task (Phase 2 task 5).
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

**Memory return.** Empty slabs and runs are dirty and are returned with
`MADV_DONTNEED` in batches through `process_madvise` along jemalloc's decay
curve: about half of what went empty at once within half of `dirty_decay_ms`,
all of it by the end of the window. Decay
runs from the allocating thread's slow path and, when enabled through the
jemalloc `background_thread` control, from a background thread. Under a
cgroup limit (`docs/design.md` section 11) freed-but-unpurged memory is
capped at 5% of `min(memory.high, memory.max)`, the decay window shortens
linearly from `dirty_decay_ms` at 70% of the limit to 1 s at 90%, and
above 90% the window is zero and the cache GC runs at every periodic tick;
a rise of `memory.events` `high` purges everything freed once. Read
`stats.cgroup.decay_ms`, `stats.cgroup.retention_cap` and
`stats.cgroup.empty_bytes` to see the policy at work. Measured in a
`docker run --memory=512m` container (`make test-cgroup`): 400 MiB
allocated, freed and idled is back to 13 MiB of RSS within 50 ms of the
frees; with `MALLOC_CONF=cgroup:false` 319 MiB is still resident 2 s later
(jemalloc 5.3.1 keeps 190 MiB, mimalloc 3.5.2 407 MiB), and a live set
swinging between 200 and 480 MiB runs for 5 minutes where jemalloc and
mimalloc are OOM-killed within 12 s.

**Kernel features, all runtime detected.** Rows marked *planned* are probed
or reserved for but not yet used by any code path.

| Feature                      | Kernel | Used for                                   | Fallback                          |
|------------------------------|--------|--------------------------------------------|-----------------------------------|
| `MADV_GUARD_INSTALL`         | 6.13   | Guard slabs and guards after huge runs     | `PROT_NONE` page after huge runs; no guard slabs |
| `process_madvise` on self    | 5.10   | Batched purge                              | Loop over `madvise`               |
| `PR_SET_VMA_ANON_NAME`       | 5.17   | Named heap mappings in `/proc/pid/maps`    | Skip                              |
| `mseal`                      | 6.10   | Seals the never-committed parts of the reservations (the gaps between class regions, the slack around the huge and metadata regions) at init; not the regions or the region table | Skip (also when seccomp filters it, as Docker's default profile does) |
| PSI triggers                 | 6.4 (unprivileged) | Background thread arms `some 150000 1000000` on the cgroup's `memory.pressure` (or the unprivileged `some 300000 2000000`); a fire takes a sample, re-evaluates the policy and runs a decay pass at once | Sample `memory.current` on the tick; also when the write is refused (Docker's default profile mounts the cgroup read-only: `EROFS`; with `nsdelegate` on the host a container's root cgroup refuses the write: `EPERM`) |
| Transparent huge pages (planned) | any | Opt-in only; the `thp` key is accepted and inert | Off                       |
| rseq `mm_cid`                | 6.3    | Optional per-CPU mode, later phase         | Thread caches only                |

Chainguard OS ships Linux 6.18, so everything above except the per-CPU mode is
available there. Containers run on whatever kernel the host has, so every path
is probed at init and degrades without changing observable behaviour; note that
a kernel before 6.13 (Docker Desktop's 6.10, for one) has no guard slabs.

**Security properties versus jemalloc.**

| Property                                 | jemalloc 5.3 | challoc         |
|------------------------------------------|--------------|-----------------|
| Metadata separate from user memory       | No           | Yes             |
| Free-list pointers in freed blocks       | Yes          | None            |
| Invalid free detected                    | No           | Yes, aborts (1) |
| Double free detected                     | Debug only   | Yes, aborts (1) |
| Size classes in isolated address ranges  | No           | Yes, randomised |
| Guard regions                            | No           | Yes (kernel 6.13+ for guard slabs) |
| Zero on free                             | Opt-in       | Opt-in: `zero_on_free` clears a block on free (2) |
| Slot order randomisation                 | No           | Opt-in: `slot_randomize` (2)                |
| Implementation language                  | C            | Rust            |

(1) Through `free`: an invalid pointer aborts at the call, and so does a
second free of a slot that has already left the thread cache (its slot bit
is clear). A second free while the slot is still cached is caught when a
flush releases the copies to the slab: the flush groups the released half
by slab, so the copies land in one slab's batch, the release aborts on the
duplicate, and the one case where the split would release one copy and
keep the other aborts at the split. A pair the split keeps together stays in the cache until a
later flush releases it; if a `malloc` pops one copy before then, the
duplicate is not detected and the slot is live twice, unless the
`double_free_scan` tunable is set: then every free (plain and sized)
compares the slot against the four most recently cached entries and aborts
on a match at the call. It is off by default because it costs about 3..4%
on larson and up to 14% on the Valkey model on x86_64
(`bench/results/2026-09-18-*-gce-detection-gaps.md`).
Through the sized path, `sdallocx(p, size, flags)`, C23 `free_sized` and
`free_aligned_sized` (what Valkey frees with): the size is checked against
the pointer's class at the call (a size that names another class aborts,
`nallocx(size, flags) == sallocx(p)` is the contract), a slot start inside
a guard slab aborts at the call, the slot bit is not read, and any other
invalid or double free is caught when the thread cache flushes the slot to
its slab: at most a cache's worth of frees later (at once when the cache is
full or with `MALLOCX_TCACHE_NONE`). Between the call and that flush the
slot can be handed out twice, which is jemalloc's behaviour for every free;
if a `malloc` takes one copy before the flush the duplicate is not
detected. This trade is the jemalloc-parity decision of
`docs/plan.md` Phase 1.5 task 4e: the exact check costs one slab-descriptor
cache miss per free, 200 of the 273 cycles per SET that separated challoc
from jemalloc on Valkey's write path.

(2) Both keys are off by default and are the operator's choice
(`MALLOC_CONF=zero_on_free:true`, `slot_randomize:true`). `zero_on_free`
clears a slot's class-size bytes on every free (plain, sized and direct)
and a huge run's usable bytes on its release, so freed data never waits in
the cache or the slab for the next owner; it costs a `memset` per free,
which is more than the 3% hardening budget on Valkey's 1 KiB values.
`slot_randomize` makes every cache refill claim from a random bitmap word
and shuffle its batch, so the order in which one class hands out
neighbouring slots is not predictable from the previous allocations; it
works against the compaction that keeps the heap dense (design 6.4), which
is why it is not the default. The price of each on `larson`, `redis` and
the Valkey model is in `bench/results/2026-09-18-*-gce-tunables.md`.
## Compatibility

challoc exports the standard C allocation functions including C23's
`free_sized` and `free_aligned_sized`, the glibc extras such as
`malloc_usable_size`, `malloc_trim`, `mallopt`, and `mallinfo2`, and the
jemalloc surface: `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`,
`sdallocx`, `nallocx`, `mallctl`, `mallctlnametomib`, `mallctlbymib`,
`malloc_stats_print`, and the `malloc_conf` configuration string. Every
jemalloc symbol is also exported with the `je_` prefix, and the C++
`operator new`/`operator delete` forms are exported under their Itanium
names. The shared library's soname is `libchalloc.so.0`; the build can
instead produce a `libjemalloc.so.2` soname for packages that only link
against jemalloc. `challoc_version()` returns the crate version and is the
`dlsym` probe for whether challoc is the loaded allocator.

Two things are stricter than jemalloc: the size passed to `sdallocx`,
`free_sized`, `free_aligned_sized` or a sized `operator delete` must name
the class the pointer was allocated from, and `malloc_usable_size`,
`realloc` and `sallocx` on a freed or foreign pointer abort where jemalloc's
behaviour is undefined. A program that leaned on that leniency dies with
`SIGABRT` and a `challoc:` message instead of corrupting the heap.

See [docs/design.md](docs/design.md) for the full design and
[docs/plan.md](docs/plan.md) for the implementation plan.

## Status

Implemented and under test, not yet packaged. Phases 0 and 1 of
[docs/plan.md](docs/plan.md) are done: the allocator builds as a `cdylib`, its
unit tests and C programs run under `LD_PRELOAD` in the dev container (267 +
89 tests as of Phase 1.5 task 11, plus the glibc and mimalloc suites and the
fuzz driver), and the Valkey soak, Valkey test suite and Ruby suites run
against it (`make valkey-soak`, `make ruby-bench`). Phase 1.5 (the GCE
performance lab) is in its acceptance run: the release-readiness tasks of
`docs/review-2026-09-18.md` are landing and the mstress RSS row is open.
`PROGRESS.md` is the task log; `bench/results/scorecard-{aarch64,x86_64}.md`
record the benchmark trend per commit. Features marked *planned* above
(zero-on-free, slot randomisation, THP) are Phase 2; `mseal` of the
reservations' never-committed ranges, the cgroup measurement layer
(`stats.cgroup.*`), the cgroup policy and its container tests (`make
test-cgroup`) have landed.
Packaging for Wolfi is Phase 3.

## Building

challoc is Linux-only and is developed inside a Wolfi container on macOS and
Linux hosts alike:

```sh
make dev                       # shell in the Wolfi dev container, repo at /src
make build                     # cargo build --release inside the container
make run CMD='cargo test'      # any one command inside the container
make test                      # release build, then the full test suite in the container
make fuzz                      # fuzz/fuzz.c: 10M random ops on 4 threads under LD_PRELOAD
make test-host                 # pure-logic unit tests on the host
make jemalloc-compat           # libjemalloc.so.2 + jemalloc.pc in target/jemalloc-compat/stage
```

`Dockerfile.dev` defines the container. Note that a plain `apk add rust` on
`wolfi-base` currently yields a broken `rustc` because Wolfi's Rust is built
against the versioned `glibc-2.44` stream; the Dockerfile handles the swap.

Native aarch64 and x86_64 builds are both supported. Use
`--platform linux/amd64` for an emulated x86_64 build on Apple silicon when
checking portability. Emulation is not suitable for benchmarks.

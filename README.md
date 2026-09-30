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
   pressure trigger fires, thread caches collected at every periodic tick
   above 90%, `MADV_FREE` below 70% of a known limit and `MADV_DONTNEED`
   above, and `thp:always` forced to `never` under a 1 GiB limit. The
   policy is `docs/design.md` section 11 (`src/policy.rs`), exercised by
   the container tests (`make test-cgroup`).
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
batches through `process_madvise` along jemalloc's decay curve: about half
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
mislead a fragmentation ratio or an autoscaler); `lazy_free:true` makes
every return lazy, `lazy_band:false` every return eager, and
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

**Kernel and CPU features, all runtime detected.** The floor is a
compatibility promise: one binary works on kernel 5.10 and glibc 2.44 with
Wolfi's compiler baseline. The ceiling is the product: the same binary lights
whatever the host has, found by probing at init (never `-C target-cpu`), and
reports it as one line from `challoc_features()` or `malloc_stats_print`'s
`Kernel and CPU features:` (`docs/design.md` section 17). Rows marked
*reported only* are probed and reported but not yet used by any code path;
the task that will use them is named in the design.

| Feature                      | Provided by | Used for                                   | Fallback                          |
|------------------------------|--------|--------------------------------------------|-----------------------------------|
| `MADV_GUARD_INSTALL`         | kernel 6.13   | Guard slabs and guards after huge runs     | `PROT_NONE` page after huge runs; no guard slabs |
| `process_madvise` on self    | kernel 6.13 (`MADV_DONTNEED` through it, with a pidfd of self; 5.10..6.12 refuse the advice), 6.15 (`PIDFD_SELF`); hidden by Docker's default seccomp | Batched purge                              | Loop over `madvise`               |
| `process_madvise(MADV_FREE)` on self | the same, probed separately (`process_madvise_free`) | Batched lazy purge under 70% of a cgroup limit | Lazy batches loop over `madvise(MADV_FREE)` |
| `PR_SET_VMA_ANON_NAME`       | kernel 5.17   | Named heap mappings in `/proc/pid/maps`    | Skip                              |
| `MADV_POPULATE_WRITE` (`populate_write`) | kernel 5.14 | Opt-in (`prefault:true` with `background_thread:true`; design 8): the background thread commits the next fresh slab of every class that refilled since its last pass and populates it with one call before the refill that will need it, so the first writes into a fresh slab take no page faults on the allocating thread; one slab per class per pass, only while the cgroup policy shows headroom | The same pass touches one byte per page (`Prefault: on (touch per page)` in the report); off by default either way |
| `mseal`                      | kernel 6.10   | Seals the never-committed parts of the reservations (the gaps between class regions, the slack around the huge and metadata regions) at init; not the regions or the region table | Skip (also when seccomp filters it, as Docker's default profile does) |
| PSI triggers                 | kernel 6.4 (unprivileged) | Background thread arms `some 150000 1000000` on the cgroup's `memory.pressure` (or the unprivileged `some 300000 2000000`); a fire takes a sample, re-evaluates the policy and runs a decay pass at once | Sample `memory.current` on the tick; also when the write is refused (Docker's default profile mounts the cgroup read-only: `EROFS`; with `nsdelegate` on the host a container's root cgroup refuses the write: `EPERM`) |
| Transparent huge pages       | any kernel with THP; per-size knobs since 6.8 | Opt-in: `thp:auto` advises 64 KiB folios on the slab groups and metadata where the host enables `hugepages-64kB` without `madvise`-gating 2 MiB pages (never a 2 MiB page, so fork stays cheap); `thp:always` commits and advises 2 MiB chunks (at 4 KiB pages; the folio path at 16 and 64 KiB, whose PMD is 32 and 512 MiB), huge runs and metadata; `thp:never` issues `MADV_NOHUGEPAGE` on the class superblock and the metadata region, so the slab regions get no huge page on a host whose global mode is `always`, and leaves the huge region (allocations of a page or more) to the host's policy, as jemalloc's `never` leaves its large allocations (the `thp:` report line says `nohuge 2 (classes, metadata; huge follows host)`); `thp_process:advised` issues `prctl(PR_SET_THP_DISABLE, 1, PR_THP_DISABLE_EXCEPT_ADVISED)` once at init (kernel 6.18, probed by the call's return: the feature line's `thp_except_advised=yes|no|inherited`), so the whole process gets huge pages only where a range was `MADV_HUGEPAGE`d; the report's `thp:` line names the host's global mode and whether the prctl is in force | Off (`thp:never`, the default); `thp_process:default` issues nothing, and `advised` on a kernel before 6.18 is skipped with the reason in the report |
| rseq `mm_cid` (`rseq`, `rseq_size`, `mm_cid`) | kernel 6.3 (`AT_RSEQ_FEATURE_SIZE >= 28`) and glibc 2.35+'s registered area, read from the `__rseq_size`/`__rseq_offset`/`__rseq_flags` symbols at init | Per-CPU caches and arenas keyed by the running thread's concurrency id, pop and push in rseq critical sections (`percpu_arena`: `auto`, `percpu`, `off`; design 15); the report keys give the probe's verdict (`mm_cid=yes` on the 7.0 GCE VMs) | Thread caches only (`mm_cid=no` on a kernel that reports no feature size, Docker Desktop's 6.10 among them; `rseq=no` without a registration) |
| rseq time slice extension  | kernel 7.0 with `CONFIG_RSEQ_SLICE_EXTENSION` (x86_64; arm64 lacks its dependencies through master), glibc 2.44 | A lock holder asks the kernel to defer a preemption until the release: `slice_ctrl.request` set after the acquire and cleared before the release, `rseq_slice_yield` after a granted release (`slice_ext`: `auto`, `on`, `off`; design 17). The feature line's `slice_ext=` key gives the kernel's answer and what is in force: `on`, `available` (the kernel has it, the key turned it off), `kernel-disabled` (the option compiled out: Ubuntu's 7.0 GCE kernels), `no-slice_ctrl` (before 7.0), `no-rseq`, `prctl-failed` | The lock without the request (one relaxed load per hold); `Slice extension: off (<why>)` in the report and the same word after `slice_ext=` in the feature line |
| MTE `mte`, `mte3` (reported only) | arm64 CPU (Armv8.5) and kernel, `AT_HWCAP2` | Tagged pointers, designed (`docs/design.md` section 18), implementation gated on a host that exposes it | Untagged pointers |
| PAC `pac` (reported only)    | arm64 CPU (Armv8.3) and kernel, `AT_HWCAP` | Signed cache pointers, a later task | Plain pointers |
| LSE `lse`, LSE2 `lse2` (reported only) | arm64 CPU (Armv8.1, 8.4), `AT_HWCAP` | 16-byte descriptor atomics, a later task; the atomics emitted today follow the compiler baseline | 64-bit atomics, LL/SC |
| BTI `bti`, SVE `sve` (reported only) | arm64 CPU and kernel, `AT_HWCAP`/`AT_HWCAP2` | Nothing; recorded for the matrix | None needed |
| LAM `lam` (reported only)    | x86_64 CPU (`cpuid` 7.1) and kernel 6.4 | Tagged pointers, designed (section 18.3), gated as MTE | Untagged pointers |
| UAI `uai` (reported only)    | AMD CPU, Zen 4 and later (`cpuid` `0x80000021`); no kernel enables it for user space yet | Tagged pointers, a later task | Untagged pointers |
| LA57 `la57`, `la57_enabled` (reported only) | x86_64 CPU (`cpuid` 7.0); the kernel booted with five-level paging (a hinted `mmap` above 2^47 is honoured) | Address-space budget for reservations and tag bits | 47-bit user addresses |
| CLDEMOTE, WAITPKG, ERMS, FSRM, AVX-512F (reported only) | x86_64 CPU (`cpuid` 7.0) | Nothing: CLDEMOTE on the release path was measured on the C4 and dropped (it cost the single-threaded rows 5..17% in cycles per operation and did not move the io-threads soak, `bench/results/2026-09-19-x86_64-lane-b-task12-cldemote.md`); WAITPKG is the lock's `umwait` hint where exposed; the rest are recorded for the matrix | None needed |
| Cache line, L1D, L2 sizes (reported only) | `CTR_EL0` (arm64), `cpuid` leaf 4 / `0x8000001D` (x86_64), `/sys/devices/system/cpu/cpu0/cache` as fallback | Geometry defaults, a later task | `unknown`; the layout constants stay fixed |

Chainguard OS ships Linux 6.18, so every kernel row above except the per-CPU
mode is available there. Containers run on whatever kernel the host has, so
every path is probed at init and degrades without changing observable
behaviour; note that a kernel before 6.13 (Docker Desktop's 6.10, for one) has
no guard slabs. Of the reference hosts, the C4A Axion VM exposes PAC, LSE2,
BTI and SVE but no MTE, and the C4 Xeon 8581C exposes LA57 (enabled),
CLDEMOTE, ERMS, FSRM and AVX-512F but no LAM or WAITPKG
(`bench/results/2026-09-19-feature-report.md`).

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
| Signed allocator pointers (arm64 PAC)    | No           | Opt-in where the CPU has it: `pac_pointers`, cache entries and the TLS heap pointer `pacda`-signed, authenticated on every `malloc` (3) |
| Tagged pointers                          | No           | Planned, hardware-gated: `tag_pointers` (4) |
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
detected. This trade is made for jemalloc parity: the exact check costs
one slab-descriptor cache miss per free, 200 of the 273 cycles per SET that
separated challoc from jemalloc on Valkey's write path.

(3) On an arm64 host whose kernel exposes pointer authentication
(`HWCAP_PACA`/`PACG`: Google Axion, Apple silicon; not Ampere Altra, not
x86_64), every pointer a thread cache holds is stored signed with the data
key A (`pacda`, the cache's stack address as the modifier) and
authenticated (`autda`) when `malloc` pops it, and the heap pointer in TLS
is signed with the thread pointer; so a write into the allocator's
metadata region cannot make `malloc` hand out a chosen address: the pop
faults (CPUs with FEAT_FPAC, Neoverse V2, Apple M2+) or returns a
non-canonical pointer that faults on first use (Neoverse N1, Apple M1).
Probed at run time, one predicted branch on a byte the fast paths load
anyway, the same binary everywhere; `MALLOC_CONF=pac_pointers:true` turns
it on. It is off by default because the two PAC instructions an entry
costs (one signing it, one authenticating it) are 11% of xmalloc-test's
cross-thread frees and 1..2% of larson on Axion, over the 1% budget for a
default-on hardening (`bench/results/2026-09-19-lane-a-pac-pointers.md`;
design 6.1, 6.2, 17).

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

(4) Designed, not implemented (`docs/design.md` section 18): `tag_pointers:mte` gives every block a random 4-bit MTE tag
retagged on free, so a read through a freed pointer faults in hardware;
`tag_pointers:lam` carries a six-bit class-and-generation tag in the
ignored top bits (LAM on x86_64, TBI on aarch64) that `free`, `realloc`,
`malloc_usable_size` and the sized path verify, so a stale, forged or
cross-class pointer aborts. Default `off`. No GCE CPU exposes MTE, LAM or
UAI to a guest (`bench/results/2026-09-19-cpu-matrix.md`), so the
implementation waits for a host that does.

challoc exports the standard C allocation functions including C23's
`free_sized` and `free_aligned_sized`, the glibc extras such as
`malloc_usable_size`, `malloc_trim`, `mallopt`, and `mallinfo2`, and the
jemalloc surface: `mallocx`, `rallocx`, `xallocx`, `sallocx`, `dallocx`,
`sdallocx`, `nallocx`, `mallctl`, `mallctlnametomib`, `mallctlbymib`,
`malloc_stats_print`, and the `malloc_conf` configuration string
(`docs/tuning.md` is the operator's guide: every key, how to set it
through `malloc_conf`, `/etc/challoc.conf` or `MALLOC_CONF`, the cgroup
policy, THP on `always` hosts, and how to read the report). Every
jemalloc symbol is also exported with the `je_` prefix, and the C++
`operator new`/`operator delete` forms are exported under their Itanium
names. The shared library's soname is `libchalloc.so.0`; the build can
instead produce a `libjemalloc.so.2` soname for packages that only link
against jemalloc. `challoc_version()` returns the crate version and is the
`dlsym` probe for whether challoc is the loaded allocator. challoc's one
extension of its own (`<challoc/challoc.h>`, design 13.7) is the
size-returning pair a `used_memory`-accounting consumer needs:
`challoc_malloc_usable`, `challoc_calloc_usable` and
`challoc_realloc_usable` store the block's usable size, and
`challoc_free_usable` returns it, from the classification the allocation
or the free already made, so Valkey's `zmalloc_size` query after every
allocation and before every unsized free disappears with no check dropped.

Two things are stricter than jemalloc: the size passed to `sdallocx`,
`free_sized`, `free_aligned_sized` or a sized `operator delete` must name
the class the pointer was allocated from, and `malloc_usable_size`,
`realloc` and `sallocx` on a freed or foreign pointer abort where jemalloc's
behaviour is undefined. A program that leaned on that leniency dies with
`SIGABRT` and a `challoc:` message instead of corrupting the heap.

See [docs/design.md](docs/design.md) for the full design and
[docs/benchmarking.md](docs/benchmarking.md) for how it is measured.

## Status

Implemented, tested and packaged for Wolfi. The allocator builds as a
`cdylib`; its unit tests and C programs run under `LD_PRELOAD` in the dev
container (`make test`, plus the glibc and mimalloc suites and the fuzz
driver), and the Valkey soak, the Valkey test suite and the Ruby suites run
against it (`make valkey-soak`, `make ruby-bench`). The kernel matrix
proves the floor on 5.10, 6.1, 6.8, 6.18, 7.0 and 7.2 kernels and on
64 KiB-page aarch64 (`bench/gce/README.md`). `contrib/wolfi/challoc.yaml`
builds `challoc`, `challoc-dev` and `challoc-jemalloc-compat` with melange,
and Wolfi's Valkey 9.1 and Ruby 3.4 are built and tested on challoc on both
architectures (`contrib/wolfi/`). `docs/tuning.md` is the operator's guide;
`bench/scorecard.sh` records the benchmark trend per commit
(`target/results/scorecard-<arch>.md`).
Zero-on-free and slot randomisation are opt-in; THP (`thp:auto|always`) is
off by default; `mseal` of the reservations' never-committed ranges, the
cgroup measurement layer (`stats.cgroup.*`) and the cgroup policy with its
container tests (`make test-cgroup`) are in. Tagged pointers
(`tag_pointers`) are designed (design section 18) and wait for hardware
that exposes MTE or LAM.

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

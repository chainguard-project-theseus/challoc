# Open issues: draft for the public tracker

**Temporary file.** This is the list of still-open items collected from the
prototype's loop state (PROGRESS.md, PROGRESS.d/, docs/plan.md and the three
review snapshots) before those files were deleted. Each entry is to be filed
as a GitHub issue in the public repository at release, and this file is to be
deleted before the history rewrite. The results files cited live in
`bench/results/` in the prototype archive; when filing, inline the numbers
and link the archive tag rather than a path that the release tree may not
carry.

Suggested labels: `roadmap`, `performance`, `memory`, `hardening`, `kernel`,
`testing`, `bench`, `docs`, `packaging`, `upstream`, `api`.

---

## From the cleanup plan's open-items table

### 1. Tagged pointers (MTE, LAM, UAI; software tag over TBI)

Label: `roadmap`, `hardening`

design.md section 18 is a full design (MTE per-allocation tags, LAM/UAI and
a software tag in the top byte, stripping before classification, a
`tag_pointers` key); no code exists and the key is not parsed. No GCE CPU
exposes MTE, LAM or UAI to a guest: Axion and Google's arm64 metal
(Neoverse N3) have no `mte`, no Intel family on GCE has LAM, the Genoa
guest hides UAI, and the Chainguard OS kernel has no `ADDRESS_MASKING`.
What is not gated: the software tag over TBI on aarch64 runs on the C4A
today. Cheapest correctness path: QEMU TCG (`-M virt,mte=on -cpu max`; `-cpu
max` with a 6.4+ guest for LAM). MTE numbers need AmpereOne bare metal
(Graviton4 and Cobalt 100 are each a `grep mte /proc/cpuinfo` away); LAM
needs Granite Rapids or Sierra Forest. Acceptance when a host exists:
negative tests (read through a freed pointer faults, stale-tag free aborts),
the fuzzer and the Valkey and Ruby suites under the key, cost rows; default
stays off unless every target row costs under 3%.

### 2. Type-isolated heaps for Clang's `-fsanitize=alloc-token`

Label: `roadmap`, `hardening`

An `__alloc_token_malloc(size, token)` family of entry points that maps
tokens to arenas, so allocations of different types never share a slab.
Behind a feature or tunable; must not change default behaviour or default
performance. Build one Wolfi package with it and measure against the
default on the consumer rows.

### 3. GWP-ASan-style sampled guarded allocations

Label: `roadmap`, `hardening`

Sampling-based guarded allocations for crash reports in production: a small
fraction of allocations placed on their own page with a guard after them
and a use-after-free trap on free, reported with a stack. Opt-in, no cost
to the unsampled path beyond a counter.

### 4. MTE on AmpereOne

Label: `roadmap`, `hardening`, `kernel`

`PROT_MTE` slab regions with random tags per allocation, detected at
runtime, off elsewhere. This is the hardware half of item 1 and waits for
a reachable AmpereOne bare-metal host. MTE's retag on free has the same
store pattern as `zero_on_free`; record both cost rows side by side.

### 5. Per-CPU caches through rseq `mm_cid`: idea and measurements

Label: `roadmap`, `performance`

A per-CPU cache mode was prototyped and removed before release; the code
is at the archive tag `archive/prototype-2026-09-30` (`src/heap/percpu.rs`,
the critical sections in `src/rseq.rs`, the model's `percpu_*` scenarios,
`tests/c/percpu.c`, design.md section 15 at that tag). The design: behind
`percpu_arena:percpu`, one table of `nr_cpu_ids` cache-header arrays
indexed by the running thread's rseq `mm_cid` (kernel 6.3+, glibc's
registered `__rseq_size >= 28`); pop, push, batch push, take-all and
install were hand-written `asm!` rseq critical sections on both
architectures (the `mm_cid` read inside the section, one commit store,
restart on abort); refill and flush ran outside the section through the
id's arena; a heap in the mode kept its home caches empty so the entry
points routed it on the slow path.

Against thread caches it cost larson1 +5.3%, larson10 +9.4%, xmalloc-test
+18.9%, valkey1 +4.7% on aarch64 and larson10 +16.6%, xmalloc-test
+16.1%, valkey1 +37.8% on x86_64 (5-round `ab`). Attributed: +47 (Axion) /
+33 (Xeon 8581C) instructions per operation of rseq section cost, the
design's floor. It won RSS only when threads far outnumber CPUs, and in a
quota pod `mm_cid` is bounded by the host's CPUs, not the quota. The
Ruby-idle RSS it bought is to be regained with idle-thread cache reclaim.
`percpu_arena` still parses (jemalloc's values) and has no effect. Ways to
give such a mode a hit path without touching the default: ifunc-selected
entry points, or a class/heap argument on the cold tail calls (priced with
`quick-perf`).

### 6. PostgreSQL `select500` on aarch64 behind glibc

Label: `performance`

pgbench select-only at 500 clients on the C4A: challoc 213.1k tps median
against glibc 233.1k (-9.4%) and jemalloc 229.3k, with overlapping sorted
lists. Three same-session 30 s A/B pairs read -5.2%, -6.5% and (order
swapped) +2.2%; instructions (162k vs 168k per transaction), L1d and dTLB
misses are equal, idle is 0%: the row is bound by pgbench's 16 threads
sharing 16 CPUs with 500 backends. On x86_64 the row is +3.5% ahead.
Next step: re-measure server-bound (fewer pgbench threads, or the client on
a second VM) before treating it as a regression; add to tuning.md's known
limitations if it holds.

### 7. Fork cost of per-class regions (`connect20`)

Label: `performance`, `memory`

PostgreSQL forks a backend per connection. A fresh challoc process owns 79
class regions and 251 metadata regions as separate mappings in a 54 TiB
span; each connection pays 58 `mprotect` slab commits, 2.4x the fork time,
~400 more page faults and sparse page-table teardown: 5.3 vs 3.9 ms kernel
CPU per connection on aarch64, 4.6 vs 3.6 on x86_64; +43% / +59% on
`connect20` against glibc. VmPTE is 1.3 MiB against glibc's 120 KiB because
every touched table head sits in its own PTE page. Candidates: commit a
fresh arena's first group per class in one `mprotect`, or let the child
inherit the parent's partial slabs (measure with a `connect20` A/B and
`strace -f` on the VM host); the larger fix is a slab-major interleave of
the descriptor, bitmap and owner tables, which changes the `free` path's
address arithmetic and needs the full protocol. Pss_Anon per backend is
1.7 MiB against glibc's 1.2.

### 8. Ruby threaded RSS on x86_64

Label: `memory`

The Ruby threaded soak on x86_64: peak RSS +12.9% over jemalloc (aarch64
-5.7%), final RSS 163 MiB against jemalloc's 132 (jemalloc's final figure
varies 131..168 MiB across runs depending on whether its per-arena force
purge at the last thread exit fires). Steady state with workers running:
x86_64 196 vs 179 MiB (+9.6%), aarch64 182 vs 199. The x86_64 residual is
64 KiB-slab holes across 17 arenas (30.6 vs 12.6 MiB) plus `Empty` slabs
that later exits' flushes leave in already-purged arenas. A jemalloc-shaped
alternative (prototype commit 4dc352c) met +9.8% but cost larson10 10% on
the Xeon. `narenas:1` is the lower-peak configuration (170 / 184 MiB). The
README must disclose this.

### 9. Valkey write phase on a small-L3 Xeon (C3, 8481C)

Label: `performance`

On C3 (Sapphire Rapids 8481C, small L3) the Valkey model's write phase reads
challoc 15..21% behind jemalloc, where an earlier measurement on the same
cell type read +3.2% ahead. The C4 `quick-perf` shows `valkey1-write` at
423 instructions per SET against 344.6 before, jemalloc 330.6 against
268.2: about 60 of the 78 added instructions are program-side (newer gcc,
newer Wolfi jemalloc, two flag tests the model gained), 16..22 are
challoc's own, which on that part crosses the instruction-window cliff
(MLP-bound: one DRAM miss per SET on the dict entry). Where to look: the
entry points' instruction accounting (re-measure without the per-CPU
dispatch, which is removed), and the
`divl` of the guard-slab test on the sized path (test with `valkey_model -x
K` and `guards:false`). Needs `c3-standard-192-metal` for the full PMU.

### 10. Post-delete RSS on 64 KiB-page aarch64

Label: `memory`, `docs`

On a 64 KiB-page kernel the Valkey soak's post-delete RSS is +20.5% over
bundled jemalloc at 600 s (+17.6% at 300 s, +20.0% at 1200 s); write phase
and peak are at parity. Attributed: Valkey's defragmenter stops on
`active-defrag-ignore-bytes` (100 MB, absolute) between sub-second cycles;
jemalloc's 192..448 KiB slabs waste 22 MiB more after the second cycle and
so earn one more cycle (74k moves), which is the whole gap. With the
constant at 80 MB challoc lands at +1.9% (diagnostic only). A challoc-side
fix of the compacting cache was measured and closed: the cache drains
everything the churn frees; the residue is memtier's age tail at 4..5
values per 64 KiB slab. Disposition: document as a known limitation in
tuning.md.

### 11. Plain `calloc` zeroes the requested size, not the usable size

Label: `api`, `docs`

jemalloc zeroes the whole region; challoc's `calloc` zeroes the requested
bytes, and `challoc_calloc_usable` zeroes the usable size. A consumer that
reads past its request up to `malloc_usable_size` after `calloc` sees dirty
bytes under challoc and zeros under jemalloc. Decide whether to change it
(a cost on every `calloc` whose size is not a class size) or document it in
tuning.md as a difference from jemalloc.

### 12. `MADV_FREE`d pages stay charged to `memory.current`

Label: `docs`

Under the cgroup policy's low band, freed memory is returned with
`MADV_FREE`; those pages stay charged, so the band reaches 70% sooner than
live memory would and flips to eager early. This is an error in the safe
direction and tuning.md section 4 already states it; file as a
documentation issue only if the design rewrite drops it. Related: the
`process_madvise` lazy fallback (a loop for lazy batches when
`process_madvise` refuses `MADV_FREE` but takes `MADV_DONTNEED`) has only
run in the host wiring test; no reachable kernel exercises it.

### 13. Unit test for `Shared::release_routed`'s "changed arena during a flush" abort

Label: `testing`

The abort has no unit test. Reaching it needs `release_owned` to stop at a
slab whose `SlabTable::owner` word then reads the arena it just refused,
i.e. the owner word rewritten between two loads. Needs a test hook or the
slab-protocol model (`src/model/`) extended with that step.

### 14. `umwait` spin hint never exercised on WAITPKG hardware

Label: `testing`, `performance`

Moot if the spin-hint key is removed before release. Otherwise:
`spin_hint:umwait` has never run, because no GCE guest exposes WAITPKG.
The first bare-metal Sapphire or Emerald Rapids (or any host with `waitpkg`
in `/proc/cpuinfo`) should run larson16 and xmalloc-test with `auto`
against `spin_hint:baseline`; the `UMWAIT_TICKS` deadline (1024) is a guess
to be measured then.

### 15. Relax Valkey's `arenas.quantum == 8` assertion under `USE_CHALLOC`

Label: `upstream`, `packaging`

Valkey's `allocator_defrag.c` asserts `arenas.quantum == 8` at start, so a
server forced onto the system `quantum16` `libchalloc.so.0` dies in
`allocatorDefragInit`; the package therefore ships a private `quantum8`
build under `/usr/lib/valkey/`. Relaxing the assertion under `USE_CHALLOC`
(challoc's defrag hint does not depend on the quantum) would let Valkey use
the system library. Upstream discussion with valkey-io.

### 16. Upstream `MALLOC=challoc` to valkey-io/valkey

Label: `upstream`

The contrib patch adds a `USE_CHALLOC` build (link `-lchalloc`, keep
`USE_JEMALLOC`/`HAVE_DEFRAG`, map `je_*` to challoc's exports) and switches
Valkey's accounting to challoc's size-returning entry points. Propose it
upstream once challoc is public and tagged, split into the enablement part
and the usable-size fast path.

### 17. Static-link support

Label: `packaging`

`crate-type` includes `staticlib`, but nothing tests or ships
`libchalloc.a`. If staticlib is dropped for 0.1, this issue tracks adding it
back with a static-link test. Known wrinkle: the archive carries an
undefined weak `rust_eh_personality` from the prebuilt `core`; a trivial C
program links fine, but a consumer whose link fails on it needs a no-op
export. RethinkDB showed that `challoc-jemalloc-compat-dev` cannot stand in
for a static-linking consumer.

---

## Other open items found in PROGRESS, the plan and the reviews

### 18. Chainguard OS (THP `always`): valkey1 behind jemalloc under the default `thp:never`

Label: `performance`, `kernel`

Chainguard OS ships THP `always`. challoc's class regions are committed with
`mprotect` inside a `PROT_NONE` reservation and advised `MADV_NOHUGEPAGE`
under `thp:never`, so on a 585 MiB Valkey heap challoc gets 32 MiB of huge
pages where jemalloc gets the whole heap. With the huge region left to the
host policy (the current `never`), valkey1 reads +5.1% behind jemalloc on
Axion under Chainguard OS; `thp:always` buys parity (-0.2%) for +9.5% RSS on
that heap and +39.9% on larson16's retained memory. On a `madvise` host the
same pair is at parity. Since Chainguard OS is the supported floor, look for
a form that recovers the throughput without the RSS: a key that follows the
host's `thp=always`, `always` banded by class size, or 2 MiB-aligned commits
for the large classes.

### 19. 7.2 kernel: khugepaged mTHP collapse/split loop

Label: `kernel`, `performance`

On a 7.2 host at an accelerated khugepaged pace, mTHP collapse costs every
allocator 25..40% on the Valkey model through a collapse/split loop with
the purge path (challoc `thp:always` 18.2 -> 9.0 M ops/s with only the
64 KiB order enabled; jemalloc 14.6 -> 8.9 under `64kB=always`). At the
shipped pace (4096 pages per 10 s) it is 160x slower and was never measured
over a soak. Run a Valkey soak on a 7.x host with `thp:auto` and
`thp:always` before `auto`'s "dTLB gain without fault-time cost" is used as
a design input.

### 20. JVM lull memory: an age-blind purge on a timer or at a lull

Label: `memory`

On the JVM soak challoc beats glibc as shipped on every RSS figure but not
glibc tuned the way Java images ship it: glibc with a `malloc_trim(0)`
every 5 s has a tail mean 16% / 22% below challoc's (aarch64 / x86_64). The
gap is ~300..400 MiB held at the end of each lull: dirty pages inside the
10 s decay window, 222 MiB of empty huge extents, and half-occupied slabs.
`huge_dirty_decay_ms` does not close it without CPU cost (0 closes half at
+22..26% CPU per request). The JVM calling challoc's own `malloc_trim(0)`
every 5 s measured tail mean -5.8% / -3.6% and CPU -4% / -1%. Experiment: a
`purge_interval_ms`-style key (the background thread purges the huge tier
and fully empty slabs every N s, no cache flush), or a lull trigger (huge
live pages or pool thread count falling), measured on the JVM soak with the
Valkey soak as the guard.

### 21. Retained huge blocks on Node-shaped consumers

Label: `memory`

Node.js http server rows on x86_64: final RSS +9.8% against glibc and +7.8%
against jemalloc, 62 MiB of it `challoc:huge`, the freed 1 MiB `Buffer`
backing stores retained for reuse up to `huge_retain_max` (64 MiB default).
Under the 10% bar, not attributed as a gap; if Node-shaped consumers matter
for RSS, consider a lower default or decay of retained huge blocks.
Overlaps item 20.

### 22. mstress RSS on x86_64 above jemalloc

Label: `memory`

mstress (a 0.63 s process) reads +18.6..19.5% RSS over jemalloc on x86_64
and +9.4% on aarch64. At `narenas:1` challoc equals jemalloc; the excess is
16 arenas times per-(arena, class) `Empty` and partial slabs that only their
own arena and class can reuse, against jemalloc's one dirty pool per arena.
Decay is irrelevant at that lifetime. The whole-group handoff between
arenas (`group_handoff`) did not move it. What would: a cross-class dirty
pool per arena (against per-class regions) or a per-slab handoff (against
the dense-bitmap line ownership invariant). Accepted for now; document.

### 23. larson dTLB refills per operation above jemalloc

Label: `performance`

larson reads 2.68 dTLB refills per operation at 2 MiB pages against
jemalloc's 1.75 at 4 KiB, so the gap is mostly not page geometry. The pages
one operation touches (descriptor, bitmap, set table, cache header, slab)
are the next place to look.

### 24. Valkey delete phase: the exact liveness check in `challoc_free_usable`

Label: `performance`, `api`

With the usable-size Valkey patch, the delete phase's remaining allocator
term on the C4 is the exact liveness check `challoc_free_usable` keeps: 74
cycles per command in the delete record, 101 in the write record, against
jemalloc's whole `sdallocx` at 20 / 57. The delete-phase valkey-benchmark
row reads +2.0% behind jemalloc (was +5.6%). A sized variant
(`challoc_sdallocx_usable(p, size)`, the size hint in place of the bit
test) would remove it, at the sized path's weaker (deferred) double-free
detection. A related design note on `malloc_usable_size` (it changes what a
use-after-free query does) was left for a decision.

### 25. `stats_print` is accepted but not implemented

Label: `api`

`MALLOC_CONF=stats_print:true` is accepted and reported as "has no effect
(not implemented)". Either implement it (print `malloc_stats_print` output
at exit, jemalloc's behaviour, via an atexit hook) or fold it into the
ignored jemalloc keys with the rest.

### 26. Missing `CONFIG_ANON_VMA_NAME` in the Chainguard OS kernel

Label: `kernel`

The Chainguard OS kernel has no `CONFIG_ANON_VMA_NAME`, so `anon_name=no`
and challoc's `[anon:challoc:classes]` style names never appear in `smaps`
there; RSS attribution by mapping name (the four-way comparison tooling)
does not work on the product kernel. The fallback is exercised and fine.
A request to the kernel team if the names are wanted in production.

### 27. Kernel matrix gap: no 6.13..6.15 host

Label: `testing`, `kernel`

The kernel matrix covers 5.10, 6.1, 6.8, 6.18, 7.0 and 7.2 on both
architectures plus 64 KiB pages, but no 6.13..6.15 host was reachable,
which is where `MADV_GUARD_INSTALL` (6.13) and `PIDFD_SELF` (6.15) arrive.
Add a cell when an image exists.

### 28. KeyDB defrag tests' stop rule under challoc

Label: `packaging`

KeyDB's `unit/memefficiency` defrag stop rule (Redis 6.2's `active -
allocated` over the whole allocator against 5% / 2 MB) has a floor of
8..12% under challoc: thread-cached slots until the 10 s cache GC, 16-slot
large-class slabs pinned by client buffers, slab tails. `get_defrag_hint`
answers for every slab class but recovered nothing, since the pinning
blocks are not the defragmenter's objects. KeyDB-side or a test-threshold
hunk like Valkey's; production defaults stop at once. KeyDB's `runtest`
cannot run on Wolfi as shipped (Tcl 9 only). Close if KeyDB support is
dropped.

### 29. Wolfi packaging follow-ups before the first Wolfi PR

Label: `packaging`

Before sending the package definitions to wolfi-dev/os: drop
`provides: jemalloc-dev` / `libjemalloc2` on the compat packages (RethinkDB
showed the -dev cannot stand in for a static-linking consumer); suppress the
automatic `so:libchalloc.so.0` dependency on the Valkey client tools; set
the release tag and its commit in `expected-commit`; decide the
`local-source`/`local-challoc` build options. Consumers that include
`<jemalloc/jemalloc.h>` build against `jemalloc-dev`, which conflicts with
`challoc-jemalloc-compat` on `libjemalloc.so` and `jemalloc.pc`.

### 30. Shell lint for the scripts

Label: `testing`

No `make lint`: `shellcheck` is not packaged in Wolfi, so the target and a
`.shellcheckrc` were never added. With CI, run shellcheck from its upstream
container image over `scripts/`, `bench/` and `tests/*.sh`.

### 31. Check the declared minimum Rust version in CI

Label: `testing`, `packaging`

Cargo.toml declares `rust-version = "1.94"`, the verified floor (the host's
rustc 1.94.1 builds the crate and runs the pure-logic tests; nothing older
was tried). The dev container and the Wolfi package build with 1.98. Add a
CI job on 1.94 so the declared floor stays true.

### 32. Cache flush selection cost on tiny-object churn (Python `dict_list`)

Label: `performance`

When a per-class cache overflows, `Heap::flush` used to sort the whole slot
array by address; on tiny-object churn that sort was 9..15% of the process
(glibc-simple 9.2%, Python `dict_list` with `PYTHONMALLOC=malloc` 15.6%).
It was replaced by an O(n) selection, and the target was to recover at
least half of the sort's share. glibc-simple clears it on both
architectures; `dict_list-malloc` is 1.1 (aarch64) and 0.5 (x86_64) points
short in a two-run comparison, and the x86_64 `python1-malloc` `ab` median
is 0.2 short (the means clear it). The selection itself is now 3.3..3.5%
of glibc-simple. Two ways to more: a counting sort with a per-heap scratch
buffer of `cap` pointers (8 KiB per flushing thread), or flushing at slab
granularity (whole high slabs plus part of the split slab, which gives up
the address-level flush-boundary double-free compare).

### 33. `dlopen` of challoc after process start

Label: `docs`

Loading challoc with `dlopen` after the process has started allocating is
unsupported by design (pointers from the previous allocator would reach
challoc's `free`), undetected in code, and undocumented. State it in the
README's usage section; consider detecting a foreign pointer on the first
`free` with a clear abort message.

### 34. `design.md` section 19 module list

Label: `docs`

design.md section 19 (source layout) misses `sys/cldemote.rs` and
`sys/pac.rs` and the split module directories (`heap/`, `class/`, `huge/`,
`layout/`, `purge/`, `model/`); `src/sys/mod.rs` cites "design.md section
18" for the layout where it is 19. Fix in the design rewrite.

---

## Already resolved in the tree (do not file)

- Cleanup table item "design 13.6 says RUNPATH where DT_RPATH is meant":
  design.md now says "rpath (`DT_RPATH` or `DT_RUNPATH`)".
- Fork PRs #1..#3: merged (commits 8089dc9, 5cc8a8c, 2c703cb).
- From the maintainability review's open list: the parked-waiter lock test
  now fails within a bound (cf663cb); the bench lock is released by a trap
  (dc2c711); the rseq probe test pins one thread per CPU (8ea835a); the
  x86_64 `AT_HWCAP` auxv check is aarch64-only (3e7175b); the mallctl
  `arena_keys` test retries across a concurrent `arenas.create`.
- The per-slab owner compare in `release_owned` (the other half of the
  owner-routing hardening item): landed in 049ee27, measured neutral
  (a8e8083).
- The Chainguard OS 512m churn flake: the retention-cap rule (an excess
  first seen at 70% or above is returned eagerly) is in the code and in
  tuning.md section 4.

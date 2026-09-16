# State of the art survey, September 2026

Findings that shaped the design, with sources. Dates are release or
publication dates. Items marked "unverified" were reported by a source that
could not be independently confirmed.

## Allocator landscape

- **jemalloc** was archived 2025-06-02 and the maintainer's postmortem said
  development had concluded (https://jasone.github.io/2025/06/12/jemalloc-postmortem/).
  Meta un-archived it and announced renewed stewardship on 2026-03-02
  (https://engineering.fb.com/2026/03/02/data-infrastructure/investing-in-infrastructure-metas-renewed-commitment-to-jemalloc/).
  5.3.1 was released 2026-04-13, the first release since 5.3.0 in May 2022
  (https://github.com/jemalloc/jemalloc/releases/tag/5.3.1). Redis and Valkey
  still vendor 5.3.0. tikv-jemallocator 0.7.0 moved to 5.3.1.
- **mimalloc** v3 is the recommended line; v3.5.2 shipped 2026-09-12. v3
  replaced thread-local segments with arena-allocated pages, moved page
  metadata out of line by default in 3.3.0 and all metadata in 3.4.1, and
  added THP-aware purging. Secure mode costs about 10% by the project's own
  estimate. Design: per-thread heaps, per-page sharded free lists, remote
  frees pushed onto a per-page atomic list and collected by the owner
  (https://github.com/microsoft/mimalloc).
- **tcmalloc** per-CPU mode uses rseq and indexes slabs by `mm_cid` when the
  kernel exposes it (https://github.com/google/tcmalloc/blob/master/docs/rseq.md).
  It cached a slab pointer in the `cpu_id_start` field and relied on the
  kernel rewriting it every context switch; Linux 6.19 stopped doing that and
  tcmalloc crashed (https://github.com/google/tcmalloc/issues/292,
  https://lwn.net/Articles/1070072/). The kernel now keeps legacy behaviour
  for 32-byte registrations and kills processes that write read-only fields in
  extended registrations. Lesson: read the rseq area, never write it.
- **snmalloc** 0.7.5 (2026-06-22) uses message-passing remote frees with
  BatchIt (ISMM 2024), out-of-band metadata since 0.6, and ships a jemalloc
  compatibility shim whose `mallctl` always returns `ENOENT`
  (https://github.com/microsoft/snmalloc/blob/main/src/snmalloc/override/jemalloc_compat.cc).
- **hardened_malloc** (release 2026091000): out-of-band metadata, 49 size
  classes each in a 32 GiB region at a random offset inside a 64 GiB gap,
  bitmap slot tracking, random slot selection, guard slabs, per-slab canaries,
  zero on free, two-stage quarantine. Third-party measurement puts it about 7%
  behind glibc in Firefox with everything on (StarMalloc, OOPSLA 2024,
  https://arxiv.org/html/2403.09435v1). Synacktiv analysis:
  https://www.synacktiv.com/en/publications/exploring-grapheneos-secure-allocator-hardened-malloc.
- **Scudo**: checksummed 16-byte inline headers, quarantine disabled in
  production, MTE support; WOOT 2024 showed two attacks given a strong
  corruption primitive, one left unfixed for performance
  (https://www.usenix.org/conference/woot24/presentation/mao).
- **Type isolation**: PartitionAlloc and Apple's xzone malloc do it; no
  mainstream Linux userland allocator does. Clang now has
  `-fsanitize=alloc-token` emitting `__alloc_token_malloc(size, token)`
  (https://clang.llvm.org/docs/AllocToken.html); the first consumer is a
  kernel patch series still under review as of May 2026.
- **MTE on servers**: only AmpereOne exposes it to userland with confirmed
  numbers, storing tags in ECC bits at about 7.6% geomean SPECrate cost
  (https://arxiv.org/html/2511.17773v1). Graviton4 and Grace do not list MTE.
  glibc 2.44 removed its `--enable-memory-tagging` option and the AArch64
  code it gated.
- **Rust-native allocators**: ferroc (nightly, semi-dormant), rusty_alloc
  (new, self-reported numbers), rsmalloc (alpha, rseq per-CPU, uses
  `__rseq_offset`). None is a mature drop-in. `#[thread_local]` and
  `-Z tls-model` remain nightly-only as of Rust 1.94. Naked functions are
  stable since 1.88.
- **Benchmarks**: mimalloc-bench is maintained and injects allocators by
  `LD_PRELOAD` with `--external`, but publishes no current results; the last
  first-party comparison is from 2021 (https://github.com/daanx/mimalloc-bench).

## glibc

- Replacement rules: must provide `malloc`, `free`, `calloc`, `realloc`;
  should provide `aligned_alloc`, `free_aligned_sized`, `free_sized`,
  `malloc_usable_size`, `memalign`, `posix_memalign`, `pvalloc`, `valloc`.
  TLS must use the initial-exec model. Must not call `fopen`, `opendir`,
  `dlopen`, `pthread_setspecific` from allocation paths. `free` preserves
  `errno` (https://sourceware.org/glibc/manual/latest/html_node/Replacing-malloc.html).
- ld.so still uses a minimal allocator before relocation and switches to the
  interposed one afterwards; its minimal `free` only handles its own last
  block (`include/rtld-malloc.h`).
- 2.41 (2025-01): extensible rseq ABI; `__rseq_size` covers `node_id` and
  `mm_cid` on kernels 6.3+.
- 2.42 (2025-07): tcache extended to 4 MiB blocks; `MADV_GUARD_INSTALL`
  used for pthread stack guards.
- 2.43 (2026-01-23): `mseal` wrapper; C23 `free_sized` and
  `free_aligned_sized`; 2 MiB THP default in malloc on AArch64; dumped heaps
  removed; memalign integer overflow CVE-2026-0861 fixed
  (https://sourceware.org/pipermail/libc-alpha/2026-January/174374.html).
- 2.44 (2026-07-25): `/etc/tunables.conf`; `glibc.elf.thp`; THP page size
  cap in malloc; documentation now allows future relaxation of alignment for
  small allocations; memory tagging removed
  (https://sourceware.org/pipermail/libc-alpha/2026-July/179159.html).
- `__libc_single_threaded` since 2.32; `sched_getcpu` is rseq-backed since
  2.35; `getrandom` uses the vDSO since 2.41 on kernels 6.11+.

## Linux kernel

| Feature                         | Version | Notes                                                                 |
|---------------------------------|---------|-----------------------------------------------------------------------|
| `process_madvise`               | 5.10    | Any advice on own mm; `PIDFD_SELF` from 6.14                          |
| `MADV_POPULATE_READ/WRITE`      | 5.14    |                                                                       |
| `PR_SET_VMA_ANON_NAME`          | 5.17    |                                                                       |
| `MADV_DONTNEED_LOCKED`          | 5.18    |                                                                       |
| `MADV_COLLAPSE`                 | 6.1     |                                                                       |
| rseq `node_id`, `mm_cid`        | 6.3     |                                                                       |
| PSI triggers unprivileged       | 6.4     | Window multiple of 2 s, evaluated by the slow averages worker         |
| `mseal`                         | 6.10    | Blocks munmap, mremap, mprotect; destructive madvise blocked on non-writable sealed anon |
| `MAP_DROPPABLE`                 | 6.11    | Not used                                                              |
| `MADV_GUARD_INSTALL/REMOVE`     | 6.13    | Anonymous only; file-backed from 6.15; no VMA split; not on mlocked   |
| Batched TLB flush for DONTNEED  | 6.16    | No API change                                                         |
| `PR_THP_DISABLE_EXCEPT_ADVISED` | 6.18    |                                                                       |
| rseq event optimisation, `VM_MAYBE_GUARD` | 6.19 | Broke tcmalloc; see above                                        |

Sources: https://docs.kernel.org/userspace-api/mseal.html,
https://lwn.net/Articles/1011366/, https://docs.kernel.org/accounting/psi.html,
https://docs.kernel.org/admin-guide/cgroup-v2.html.

## cgroups

- `memory.max.effective` was proposed in 2024 and never merged; the effective
  limit requires walking ancestors. .NET does this walk; Go 1.25 reads only the
  leaf for `GOMAXPROCS`; the JVM uses `UseContainerSupport`.
- `MADV_FREE` pages stay charged to `memory.current` until reclaim discards
  them; they are reclaimable before OOM, but their presence in `current`
  misleads anything that reads it.
- No cgroup references exist in jemalloc, mimalloc, or glibc malloc; tcmalloc
  only reads cpusets for rseq.

## jemalloc API dependence

Redis and Valkey (`src/zmalloc.c`, `src/allocator_defrag.c` in Valkey,
`src/defrag.c` and `src/object.c` in Redis):

- Symbols: `je_malloc`, `je_calloc`, `je_realloc`, `je_free`,
  `je_posix_memalign`, `je_mallocx`, `je_rallocx`, `je_dallocx`,
  `je_sdallocx`, `je_malloc_usable_size`, `je_mallctl`,
  `je_mallctlnametomib`, `je_mallctlbymib`, `je_malloc_stats_print`,
  `je_malloc_conf` (defined by Redis as
  `lg_tcache_nslots_mul:3,tcache_nslots_small_max:1000`).
- Keys: `epoch`, `stats.allocated`, `stats.active`, `stats.resident`,
  `stats.retained`, `stats.arenas.<i>.pmuzzy`, `stats.arenas.<i>.pactive`,
  `stats.arenas.<i>.small.resident`, `stats.arenas.<i>.small.allocated`,
  `stats.arenas.<i>.large.allocated`, `arenas.page`, `arenas.nbins`,
  `arenas.quantum`, `arenas.bin.<j>.size`, `arenas.bin.<j>.nregs`,
  `stats.arenas.<i>.bins.<j>.curregs`, `stats.arenas.<i>.bins.<j>.curslabs`,
  `stats.arenas.<i>.bins.<j>.nonfull_slabs`, `arenas.narenas`,
  `arena.<i>.purge`, `background_thread`, `arenas.create`, `tcache.create`,
  `experimental.utilization.batch_query` (Valkey defrag).
- Valkey's defrag asserts `arenas.quantum == 8`, moves an allocation when its
  slab is below average utilisation by a 12.5% margin or below 1/8 full, and
  skips bins with fewer than two non-full slabs. It reallocates with
  `MALLOCX_TCACHE_NONE` so the new block lands in a full-ish slab.
- Fragmentation math in `INFO memory`: `allocator_frag_ratio =
  frag_smallbins / allocated + 1` where `frag_smallbins = sum over bins of
  ((nregs * curslabs) - curregs) * size`; `allocator_rss_ratio =
  resident / active`; `mem_fragmentation_ratio = process_rss / used`.
- Build: bundled 5.3.0 with `--with-jemalloc-prefix=je_ --with-lg-quantum=3
  --disable-cache-oblivious`. No upstream system-jemalloc option; Debian
  carries a `USE_SYSTEM_JEMALLOC` patch
  (https://salsa.debian.org/debian/valkey/-/raw/master/debian/rules), and with
  system jemalloc Redis loses defrag because it needs the `get_defrag_hint`
  patch. Alpine builds `MALLOC=libc`, losing defrag, purge, stats, and the
  background thread.
- Wolfi: `valkey-9.1.yaml` runs plain `make BUILD_TLS=yes all`, so it links
  the bundled static jemalloc; the listed `jemalloc` build dependency is
  unused. Ruby 3.x, KeyDB, and RethinkDB link the system `jemalloc-dev`.
  MariaDB sets `WITH_JEMALLOC=NO`. No Redis package exists in the tree.

Other users of the extended API: Aerospike (`jem_` prefix, `arenas.extend`,
`thread.tcache.flush`), Rspamd (`malloc_conf` with decay settings, stats
keys), ClickHouse (`arena.4096.purge`, `background_thread`,
`max_background_threads`, `prof.*`), MariaDB (`dlsym("mallctl")`, `version`),
RocksDB (`arenas.create`, `arena.N.extent_hooks`, not supported by challoc),
tikv-jemallocator (`_rjem_` prefix, stats keys, `thread.allocatedp`). Ruby,
Varnish, Ceph link only.

jemalloc ABI facts: soname `libjemalloc.so.2`; flags `lg_align` in bits 0-5,
`ZERO` 0x40, `TCACHE(t) = (t + 2) << 8` with `TCACHE_NONE = 1 << 8`,
`ARENA(a) = (a + 1) << 20`; `MALLCTL_ARENAS_ALL = 4096`
(https://jemalloc.net/jemalloc.3.html).

# Verus results

Figures as of main `a1165e7f` (2026-11-01).

## Headline

challoc is a memory-safe, jemalloc-compatible malloc for Wolfi and Chainguard OS, written in `no_std` Rust. It exports the C allocation functions, glibc extras, and jemalloc’s extended API, including `mallocx`, `sdallocx`, `mallctl`, and `malloc_stats_print`.

Verus verifies **92.8% of executable lines** and **82.5% of executable functions**. Counting trusted functions whose full contracts are checked outside Verus raises coverage to **93.6% of lines** and **84.9% of functions**.

`theorem_metadata_integrity` proves that the bytes of a returned block (its requested size, at least 1) are separate from the metadata ranges named in its contract and the process’s `Shared` static, subject to its [stated exclusions](../src/heap/mod.rs).

`Live::theorem_apart` and `Live::theorem_distinct` prove that 2 live blocks have disjoint usable bytes and different non-null addresses, including when an allocation requested 0 bytes.

`theorem_no_syscall_on_fast_paths` proves that the named allocation and free fast paths make no system call, except the write and `SIGABRT` of an integrity abort on a corrupt PAC-signed cache entry.

The 5 `theorem_valid_*_free_never_aborts` proofs show that freeing a live block through their covered paths never reaches a not-an-allocation, invalid-free, or double-free check; resource failures and integrity checks remain possible.

`theorem_no_user_reads` proves that the named allocation and free cores do not read a user block’s bytes, except for opt-in debug zero checks.

`theorem_read_only_never_aborts` proves that its covered read-only entry points abort at most at an integrity check. `theorem_mallctl_reads_never_abort` and `theorem_mallctlbymib_reads_never_abort` prove that `mallctl` and `mallctlbymib` never abort for read-only keys other than the 4 `thread.*` readers.

`theorem_zeroed_memory`, `theorem_zero_on_free`, and `theorem_zero_on_free_huge` cover zeroed results and zero-on-free over the whole usable size.

`theorem_exact_stats` proves that, at a quiescent state, reported allocated bytes equal the usable bytes of live blocks plus cached slots.

`theorem_guard_pages`, `theorem_guard_free`, and `Inner::theorem_run_guards` keep the covered blocks and free targets out of guard slabs and huge-run usable pages out of live-run guard pages.

Every theorem is conditional on the [trust base](#trust-base) and the caller obeying the C allocation contract. Executable theorems marked “Verification only” use `cfg(verus_keep_ghost)` and are absent from the shipped build.

## Theorems

The 58 `theorem_*` functions comprise 44 proof functions and 14 verification-only executable functions. The count takes every function definition under `src/` whose name starts with `theorem_`, including methods such as `Live::theorem_apart`; it leaves out the 4 `fn theorem_` strings that the restated-body tests in `src/api/restated.rs` search for. The “Assumes” column names the main premises for each result; it is not a substitute for the function’s full `requires` contract. In particular, C entry points rely on the caller’s contract, and proofs about allocator state rely on the state invariants and trusted seams they use.

### API and C boundary

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `theorem_no_syscall_on_fast_paths` | The covered `malloc` and operator-new, `free`, `free_sized` or flagless `sdallocx`, and `challoc_free_usable` fast paths make no syscall except an integrity abort’s write and `SIGABRT`. | [api/libc.rs](../src/api/libc.rs) | `integrity_aborts`; a block token from `axiom_c_claim`; a restatement of the fast paths. |
| `theorem_no_user_reads` | The covered allocation and free cores do not read user block bytes, except opt-in debug zero checks. | [api/libc.rs](../src/api/libc.rs) | `may_abort`, `may_syscall`, and the caller’s block token. |
| `theorem_read_only_never_aborts` | Covered calls to `nallocx`, `challoc_features`, `mallinfo2`, `get_defrag_hint`, `malloc_usable_size(NULL)`, and `experimental.utilization` abort at most at an integrity check. | [api/mod.rs](../src/api/mod.rs) | `may_syscall`, `integrity_aborts`, `shared.wf`, and a restatement of covered calls. Read-only `mallctl` and `mallctlbymib` keys are covered by `theorem_mallctl_reads_never_abort` and `theorem_mallctlbymib_reads_never_abort`. |
| `theorem_realloc_to_usable_size` | `realloc(p, malloc_usable_size(p))` returns `p` exactly when `p` is a slot or a run with more than `MEDIUM_MAX` usable bytes. | [api/libc.rs](../src/api/libc.rs) | A live block token. |
| `theorem_valid_lookups_never_abort` | Lookups of a live block by `malloc_usable_size` and `realloc` avoid the not-an-allocation, invalid-free, and double-free checks. | [api/libc.rs](../src/api/libc.rs) | A live block token; excludes allocation after a move and freeing the old block. |
| `theorem_nallocx_zero` | `nallocx(size, flags)` is 0 exactly when no class serves the requested size and alignment and no huge run can be placed. | [api/jemalloc.rs](../src/api/jemalloc.rs) | `regions_ok`. |
| `theorem_nallocx_zero_rejects` | A request for which `nallocx` returns 0 makes `mallocx` return `NULL` in every allocator state. | [api/jemalloc.rs](../src/api/jemalloc.rs) | `may_abort` and `may_syscall`. |
| `theorem_sized_slot_nallocx` | A normally returning sized free of a slot received a size whose `nallocx` equals the slot’s class size. | [api/jemalloc.rs](../src/api/jemalloc.rs) | A valid slot and size hint. |
| `theorem_sized_run_nallocx` | The same size agreement holds for huge runs at alignments that leave the size huge and placeable. | [api/jemalloc.rs](../src/api/jemalloc.rs) | A live huge run and the stated alignment conditions. |
| `theorem_mib_round_trip` | A mallctl name maps to a management information base (MIB) and back to the same node; the split is unique, and numeric levels contain their component’s decimal value. | [api/mallctl.rs](../src/api/mallctl.rs) | The name, components, MIB, and node contracts. |
| `theorem_mallctl_reads_never_abort` | A `mallctl` call for a read-only key other than the 4 `thread.*` readers, or for a utilization query, never aborts. | [api/mallctl.rs](../src/api/mallctl.rs) | `may_syscall`, `shared.wf`, a `ReadyMark` for a `READY` the caller saw, the call’s C token, and a restatement of the body after `shared_published`. |
| `theorem_mallctlbymib_reads_never_abort` | A `mallctlbymib` call for a MIB of a key or query that `theorem_mallctl_reads_never_abort` covers never aborts. | [api/mallctl.rs](../src/api/mallctl.rs) | The same premises, with the `mallctlbymib` call’s C token and a restatement of its body. |

### Free paths

All 5 proofs in [heap/paths.rs](../src/heap/paths.rs) assume `may_syscall`, `kernel_aborts`, `pending_aborts`, `shared.wf`, and a live block token. The direct paths also assume `integrity_aborts`.

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `theorem_valid_free_never_aborts` | Freeing a live block without a cache avoids the not-an-allocation, invalid-free, and double-free checks. | [heap/paths.rs](../src/heap/paths.rs) | Common free-path premises; direct-path integrity premise. |
| `theorem_valid_run_free_never_aborts` | Freeing a live huge run on a thread with a heap avoids those checks. | [heap/paths.rs](../src/heap/paths.rs) | Common free-path premises and a heap. |
| `theorem_valid_cache_free_never_aborts` | Freeing through caches, including garbage collection, decay, purge, arena build, and flush slow paths, avoids those checks. | [heap/paths.rs](../src/heap/paths.rs) | Common free-path premises and a heap. |
| `theorem_valid_sized_free_never_aborts` | A sized free with a correct size hint avoids those checks. | [heap/paths.rs](../src/heap/paths.rs) | Common free-path premises, a heap, and a correct hint. |
| `theorem_valid_sized_direct_free_never_aborts` | A sized free without a cache and with a correct size hint avoids those checks. | [heap/paths.rs](../src/heap/paths.rs) | Common free-path premises, a correct hint, and the direct-path integrity premise. |

### Blocks and metadata

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `theorem_region_separation` | An allocated block lies wholly in a class region or the huge region, outside the metadata region. | [heap/mod.rs](../src/heap/mod.rs) | Region layout and a valid block. |
| `theorem_metadata_integrity` | A returned non-null block shares no byte with the covered metadata ranges or the process’s `Shared` static. | [heap/mod.rs](../src/heap/mod.rs) | Published `Shared::wf`, 3 kernel `mmap` reservations, Meta lock axioms, and trusted descriptor-table storage and bitmap constructors; see its exclusions in [Known limits and open work](#known-limits-and-open-work). |
| `theorem_guard_pages` | No allocated block lies in a guard slab. | [heap/mod.rs](../src/heap/mod.rs) | A valid block and the class-region layout. |
| `theorem_guard_free` | No valid free target lies in a guard slab. | [heap/mod.rs](../src/heap/mod.rs) | A valid free target and the class-region layout. |
| `theorem_free_target_aligned` | Every valid free target is quantum-aligned. | [heap/mod.rs](../src/heap/mod.rs) | A valid free target. |
| `theorem_no_block_at_zero` | `NULL` is neither a block nor a free target. | [heap/mod.rs](../src/heap/mod.rs) | The block and free-target definitions. |
| `Live::theorem_apart` | The usable bytes of 2 live blocks are disjoint. | [heap/live.rs](../src/heap/live.rs) | Tokens for 2 live blocks. |
| `Live::theorem_distinct` | The addresses of 2 live blocks are different and non-null. | [heap/live.rs](../src/heap/live.rs) | Tokens for 2 live blocks. |
| `Live::theorem_interior` | An interior pointer into a live block is not a free target, except a page start inside a huge run. | [heap/live.rs](../src/heap/live.rs) | A live block; the C caller contract excludes freeing the huge-run exception. |
| `Live::theorem_purge_huge` | A huge purge does not discard the usable bytes of a live block. | [heap/live.rs](../src/heap/live.rs) | A live block and the huge-run authority. |

### Caches

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `CachePerms::theorem_uniqueness_cached` | 2 cached entries of 1 class in 1 heap name different slots. | [heap/mod.rs](../src/heap/mod.rs) | Cache permissions. |
| `Heap::theorem_uniqueness` | 2 cached entries of 1 class in 1 heap name different slots. | [heap/mod.rs](../src/heap/mod.rs) | A well-formed heap and its cache permissions. |
| `CachePerms::theorem_uniqueness_held` | A cached slot belongs to no other holder: a free bitmap, another heap, an in-flight claim, or a live block. | [heap/mod.rs](../src/heap/mod.rs) | Cache permissions and holder state. |
| `Heap::theorem_uniqueness_held` | The heap-level holder proof gives the same exclusion for a cached slot. | [heap/mod.rs](../src/heap/mod.rs) | A well-formed heap and the cache-permission proof. |
| `Heap::theorem_uniqueness_live` | A cached slot is not a live block. | [heap/mod.rs](../src/heap/mod.rs) | A well-formed heap and live-block authority. |
| `Heap::theorem_uniqueness_apart` | 2 heaps never cache the same slot. | [heap/mod.rs](../src/heap/mod.rs) | Well-formed heaps and cache authorities. |
| `Heap::theorem_cache_routing` | Cache routing keys are distinct, a header routes to at most 1 arena, and pops do not take from the `FOREIGN` node. | [heap/mod.rs](../src/heap/mod.rs) | A well-formed heap and its routing state. |
| `theorem_exit_no_rebuild` | At a TLS slot holding `TLS_EXITED`, which the thread’s heap destructor stores before freeing the heap, the slot readers hand out no heap and create none, and every value a slot write may store over it (`slot_step`, required by `tls::set`) is `TLS_EXITED`. Verification only. | [heap/thread.rs](../src/heap/thread.rs) | The slot premise of `TlsSlot::entry`, whose call sites a source test checks; trusted nested-entry specs. |

### Zero and usable size

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `theorem_zero_on_free` | Zero-on-free covers a slot’s whole usable size. | [heap/zero.rs](../src/heap/zero.rs) | A `Zeroed` token, including trusted fresh-memory facts. |
| `theorem_zero_on_free_huge` | Zero-on-free covers a huge run’s whole usable size. | [heap/zero.rs](../src/heap/zero.rs) | A `Zeroed` token and a live run. |
| `theorem_zeroed_memory` | A calloc-style result reads zero over its whole usable size. | [vspec/zero.rs](../src/vspec/zero.rs) | A `Zeroed` token, including trusted facts that fresh anonymous memory reads zero. |
| `theorem_usable_consistent` | A slot’s `malloc_usable_size` equals the usable size stored by `challoc_malloc_usable`. | [vspec/usable.rs](../src/vspec/usable.rs) | A valid slot and its stored size. |
| `theorem_stored_zero_iff_null` | The stored usable size is 0 exactly when the pointer is `NULL`. | [vspec/usable.rs](../src/vspec/usable.rs) | The stored-size contract. |

### Slabs

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `Proto::theorem_quiescent` | At a quiescent state, the live count equals the set bits; an Active slab outside the partial set is full. | [class/complete.rs](../src/class/complete.rs) | A quiescent slab state. |
| `Proto::theorem_dry` | If idle sets are empty and partial slabs are full, every committed slab is Active and full. | [class/complete.rs](../src/class/complete.rs) | The stated idle and partial-set conditions. |
| `ClassState::theorem_claim_complete` | A refill that reserved nothing leaves no free slot in any committed slab of that arena. | [class/complete.rs](../src/class/complete.rs) | A refill that reserved nothing and the slab-state model. |
| `theorem_shuffle_permutation` | Slot shuffling loses no slot and duplicates none. | [class/mod.rs](../src/class/mod.rs) | The shuffle’s input and output sequences. |

### Huge runs

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `RunToken::theorem_purge_apart` | A byte a huge purge may discard belongs to no page, including a guard page, of a live run. | [huge/mod.rs](../src/huge/mod.rs) | A run token and live-run authority. |
| `theorem_huge_stats` | `stats.runs` and `run_pages` equal the live runs and their pages. | [huge/mod.rs](../src/huge/mod.rs) | A well-formed huge-region state. |
| `theorem_run_counted` | A live run’s extent and pages appear in the run authorities. | [huge/mod.rs](../src/huge/mod.rs) | A live run token and huge-region state. |
| `Inner::theorem_free_exact` | A huge free-bitmap bit is set exactly for a page in a free extent; none is set past the high-water mark. | [huge/mod.rs](../src/huge/mod.rs) | A well-formed huge-region state; `Huge::audit` checks the same property at run time. |
| `Inner::theorem_coalesced` | No 2 free extents touch. | [huge/mod.rs](../src/huge/mod.rs) | A well-formed huge-region state. |
| `Inner::theorem_run_guards` | A live run’s usable pages overlap no live run’s guard pages. | [huge/mod.rs](../src/huge/mod.rs) | A well-formed huge-region state and live runs. |
| `Huge::theorem_placeable_fits_empty` | A placeable request fits an empty huge region; an unplaceable request fails in every state. | [huge/mod.rs](../src/huge/mod.rs) | The request’s placeability conditions and region geometry. |

### Purge and decay

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `theorem_purge_well_formed` | A purge range is non-empty, page-aligned, inside a huge or class region, and outside metadata and guard slabs. | [purge.rs](../src/purge.rs) | A valid purge range and region layout. |
| `Decayer::theorem_decay_deadline` | A deadline is no earlier than now and no more than 1 step of the current period away; it does not return a stale, longer `next_walk`. | [purge.rs](../src/purge.rs) | The decayer’s current period and timing state. |
| `Decayer::theorem_decay_deadline_under_pressure` | Higher usage cannot produce a later deadline bound. | [purge.rs](../src/purge.rs) | Comparable usage states and the pressure-period rule. |
| `theorem_pressure_never_lengthens_decay` | Higher usage cannot lower the pressure band or lengthen the decay period. | [policy.rs](../src/policy.rs) | Comparable usage at the stated limit. |

### Statistics

| Theorem | Statement | File | Assumes |
| --- | --- | --- | --- |
| `Books::theorem_quiescent` | At a quiescent state, each class’s claimed minus released slots equals cached plus live slots; large words equal live runs and pages. | [stats/ledger.rs](../src/stats/ledger.rs) | A quiescent ledger and allocator state. |
| `theorem_exact_stats` | Reported allocated bytes equal live blocks’ usable bytes plus cached slots’ bytes at a quiescent state. | [stats/ledger.rs](../src/stats/ledger.rs) | A quiescent ledger and the accounting invariants. |

## Vulnerability classes ruled out

Each row names a bug class motivated by real CVEs in other allocators and the entry-point contracts on main that exclude it for the named entry points; challoc never had these bugs. As with every theorem here, these claims depend on the [trust base](#trust-base) and callers obeying the C allocation contract. `heap::fits(r, size, align)` means a non-null result is a slot start of a class at least `size` bytes large, or a huge-region page start with `size` bytes of the region from it, at an address that is a multiple of `align`; `size` is an unbounded specification integer, so `n * m` does not wrap.

| Class | Ruled out by | Entry points covered | Motivating CVEs |
| --- | --- | --- | --- |
| Multiplication overflow in `calloc` and `reallocarray` | The exports ensure `n * m > usize::MAX ==> r@.addr == 0` and `heap::fits(r as *mut u8, n * m, QUANTUM as int)`. [api/libc.rs](../src/api/libc.rs) | `calloc`, `je_calloc`, `reallocarray` | The bug class behind [CVE-2006-7252](https://nvd.nist.gov/vuln/detail/CVE-2006-7252) (FreeBSD and NetBSD jemalloc `calloc` allocated 1 byte after an integer overflow). |
| Alignment and page-rounding overflow | `aligned_alloc` and `memalign` ensure `!is_pow2(align) ==> r@.addr == 0` and `is_pow2(align) ==> heap::fits(r as *mut u8, size as int, align as int)`. `valloc` and `pvalloc` ensure `heap::fits(r as *mut u8, size as int, (1usize << crate::layout::process_regions().lg_page) as int)`. `pvalloc_block` rounds with `size.checked_add(page - 1)` and proves by a bit-vector assertion that the rounded size is at least `size`. `posix_memalign_block`, which `posix_memalign` calls, ensures `heap::fits(q.value(), size as int, align as int)` for the stored pointer on a 0 return. The `posix_memalign` exports ensure the same of `*out` as the C caller reads it after a 0 return, and that `*out` is what the caller left there after `EINVAL` or `ENOMEM` (`c_out(out) == c_out_entry(out)`), as in glibc. [api/libc.rs](../src/api/libc.rs) | `aligned_alloc`, `memalign`, `valloc`, `pvalloc`, `posix_memalign`, and their `je_` exports | The bug classes behind [CVE-2013-4332](https://nvd.nist.gov/vuln/detail/CVE-2013-4332) (glibc `pvalloc`, `valloc`, `posix_memalign`, `memalign` and `aligned_alloc` integer overflows caused heap corruption) and [CVE-2018-6485](https://nvd.nist.gov/vuln/detail/CVE-2018-6485) (glibc `posix_memalign` and `memalign` could return a heap area that is too small). |
| A request near `SIZE_MAX` returns a too-small block | `malloc` and `je_malloc` ensure `heap::fits(r as *mut u8, size as int, QUANTUM as int)`, and `alloc_huge` ensures `r.0@.addr != 0 ==> size <= shared.huge_usable(size)` ([api/libc.rs](../src/api/libc.rs)). `mallocx` and `je_mallocx` ensure `crate::heap::fits(r as *mut u8, size as int, (1usize << ((flags as u32) & 0x3f)) as int)` and that the usable size is `nallocx`’s; `theorem_nallocx_zero_rejects` proves that a request for which `nallocx` returns 0 makes `mallocx` return `NULL` in every state ([api/jemalloc.rs](../src/api/jemalloc.rs)). | `malloc`, `je_malloc`, `mallocx`, `je_mallocx` | The bug classes behind [CVE-2017-17426](https://nvd.nist.gov/vuln/detail/CVE-2017-17426) (glibc 2.26 `malloc` near `SIZE_MAX` returned a block that is too small through a tcache path with no overflow check), [CVE-2018-6551](https://nvd.nist.gov/vuln/detail/CVE-2018-6551) (glibc `malloc` near `SIZE_MAX` on powerpc and i386 returned a region smaller than requested), and [CVE-2005-4895](https://nvd.nist.gov/vuln/detail/CVE-2005-4895) (TCMalloc integer overflows allocated less memory than expected). |

In a secure-execution process (started with `AT_SECURE` set: setuid, setgid, file capabilities, or an LSM transition), `theorem_secure_conf` proves that no call of `env_var` returns, so `config::load` reads neither `MALLOC_CONF` nor `CHALLOC_CONF` ([config.rs](../src/config.rs)). That closes the shape of [CVE-2023-4911](https://nvd.nist.gov/vuln/detail/CVE-2023-4911) (glibc’s `GLIBC_TUNABLES`, malloc tunables among them, parsed by a setuid program’s loader). The theorem does not prove that the resulting `Config` equals the one the process would get with neither variable set, because `Config::parse_source` has no functional specification. It trusts `getauxval(AT_SECURE)` and `env_var`’s contract; a test, not Verus, checks that `env_var` is the only reader of `environ` and its glibc aliases and that `load` is its only caller outside the tests.

### Exploit techniques

These heap-exploitation techniques have no CVE of their own. Each turns an application bug, a use-after-free write or an overflow, which challoc does not prevent, into control of the allocator. Under the same trust base and C allocation contract, the theorems rule out the allocator behavior each technique relies on, for writes into a block’s bytes. A write that leaves every block and reaches metadata is the linear-overflow item in [Not ruled out yet](#not-ruled-out-yet).

| Technique | Ruled out by | Entry points covered | Write-ups |
| --- | --- | --- | --- |
| Free-list poisoning through a freed block: tcache poisoning, House of Lore, unsorted bin attack | These techniques write a forged next or bk link into a freed block’s bytes and rely on `malloc` reading it. `theorem_no_user_reads` proves the covered allocation and free cores read no user block bytes, except opt-in debug zero checks, so those cores never read the forged link ([api/libc.rs](../src/api/libc.rs)). `theorem_metadata_integrity` proves cache stack entries and the metadata allocator’s free-list link words share no byte with a returned block ([heap/mod.rs](../src/heap/mod.rs)). | `malloc`, `calloc`, `aligned_alloc`, `mallocx`, `free`, `free_sized`, `challoc_free_usable`, `dallocx`, `sdallocx`, `malloc_usable_size` | [tcache poisoning](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/tcache_poisoning.c), [House of Lore](https://github.com/shellphish/how2heap/blob/master/glibc_2.35/house_of_lore.c), [unsorted bin attack](https://github.com/shellphish/how2heap/blob/master/glibc_2.27/unsorted_bin_attack.c), and [Safe-Linking](https://research.checkpoint.com/2020/safe-linking-eliminating-a-20-year-old-malloc-exploit-primitive/) (which masks the link but can be bypassed with a heap leak). |
| Overlapping chunks: a forged chunk size makes `malloc` return a block that overlaps a live one | `Live::theorem_apart` proves that the usable bytes of 2 live blocks are disjoint ([heap/live.rs](../src/heap/live.rs)). `theorem_region_separation` proves an allocated block lies wholly in 1 class region or the huge region, outside the metadata region ([heap/mod.rs](../src/heap/mod.rs)). Both rely on callers freeing only live blocks. A slot’s size comes from its class region, not from bytes next to it, and `theorem_no_user_reads` proves the covered cores read no block bytes, so a size written into a neighboring block is never read ([api/libc.rs](../src/api/libc.rs)). | All allocations, for any 2 live blocks; region separation for results satisfying `heap::fits`. | [Overlapping chunks](https://github.com/shellphish/how2heap/blob/master/glibc_2.27/overlapping_chunks.c) (an overflow overwrites a freed chunk’s size). |
| Corrupting allocator metadata stored among user blocks: inline chunk headers, in-slot free-list pointers | `theorem_metadata_integrity` proves a returned block’s bytes share no byte with the metadata ranges its contract names: metadata region ranges, the metadata allocator’s link words, the `Shared` static, class-table descriptors, owner and bitmap words, cache headers and stack entries, huge descriptors, descriptor-table slots, and free-page bitmap words. Its [exclusions](#known-limits-and-open-work) remain. [heap/mod.rs](../src/heap/mod.rs) | Results satisfying `heap::fits` (the allocation entry points in the CVE table). | [PartitionAlloc](https://chromium.googlesource.com/chromium/src/+/main/base/allocator/partition_allocator/PartitionAlloc.md) (free-list pointers are the only inline metadata and are encoded and shadowed because overruns can reach them); [musl mallocng design notes](https://openwall.com/lists/musl/2020/05/13/1) (oldmalloc’s in-band headers versus mallocng’s out-of-band metadata). |
| Use of a thread’s heap after thread-exit teardown: a cache rebuilt after the thread’s destructor ran, or the freed heap reached again through TLS | `heap_destructor` stores `TLS_EXITED` in the thread’s TLS slot before `Heap::destroy` frees the heap (the order is read off the code). Every slot write requires `slot_step`, under which `TLS_EXITED` is final, and `theorem_exit_no_rebuild` proves that at `TLS_EXITED` the fast and slow slot readers hand out no heap and make no `Heap::create` call ([heap/thread.rs](../src/heap/thread.rs)). A signal handler or another key’s destructor that allocates on the exiting thread is served without the thread’s cache. Trusted: the slot premise (a top-level user’s token holds the slot as the last write left it; a source test checks which functions make tokens and how many, not that a nested user is lent the outer token), the specs of the `pthread_*` calls that can re-enter the allocator, and the slot’s asm accessors (partly checked by Isla). Not covered: explicit caches (`tcache.destroy`), which are not reached through the slot. | Every entry that reads the slot: the `malloc`, `free`, `free_sized`, and `challoc_free_usable` fast paths, and every path through `heap_for_thread` | glibc [BZ 22111](https://sourceware.org/bugzilla/show_bug.cgi?id=22111) and [BZ 28028](https://sourceware.org/bugzilla/show_bug.cgi?id=28028) (thread exit rebuilt the tcache after shutdown and leaked it). |

### Not ruled out yet

These stated limits and open work remain on main. [Issue #397](https://github.com/wolfi-dev/challoc/issues/397) tracks the open work.

- Double and invalid frees — stated limit: the proofs assume the C contract. [hardening.md](../docs/hardening.md#1-invalid-and-double-frees) documents that a second free of a slot still held in a thread cache is left to a flush, which catches it only if the slot is still unclaimed then (a `malloc` pop or any refill in between makes it live twice), and that a plain free of a slot start a refill claimed into a cache but `malloc` has not handed out passes the bit test; `double_free_scan` narrows that window by checking a freed slot against the four most recent entries of the cache it is pushed to.
- Use after free with same-class reuse — stated limit: challoc has no quarantine or type isolation, by design.
- A linear overflow off a block — open work, #397’s “Overflow reach”: separation from metadata and other blocks is proved, but no theorem says an overflow faults before reaching them.
- Allocator state after `fork` — open work: lock state after `fork` rests on `axiom_fork_entry`, tracked under #397’s “Theorem 33”. Thread exit is covered by `theorem_exit_no_rebuild` (exploit techniques above) up to the slot premise, which a source test checks only in part.

## Coverage

[Verus](README.md) release `0.2026.10.18.7d82994` checks every operation in a `verus!` function for arithmetic overflow, division by zero, out-of-bounds indexing, reachable panics, and loop and recursion termination. It also checks the function’s `requires` and `ensures` contract. Ghost code is erased from ordinary builds.

The [counter](count) includes executable functions under `src/` and excludes test code. It counts a function as Verus-verified when it is inside `verus!`, is neither `external_body` nor `external`, and has no `assume`, `admit`, or `Tracked::assume_new` in its body. It counts ghost functions separately. Shares are rounded down to a tenth of a percent.

| Coverage at `a1165e7f` | Functions | Lines |
| --- | ---: | ---: |
| Verus-verified | 1272 of 1541 (82.5%) | 38906 of 41901 (92.8%) |
| Trusted inside `verus!` | 251 | 2744 |
| Outside `verus!` | 18 | 251 |
| Trusted functions with a discharged seam | 37 | 346 |
| Verified plus discharged | 1309 of 1541 (84.9%) | 39252 of 41901 (93.6%) |
| Partly checked, excluded from combined coverage | 21 | 204 |
| Trusted with no check | 193 | 2194 (5.2% of lines) |
| Ghost functions, counted separately | 2062 | — |

A *discharged* seam has an outside check that covers its whole contract, under every input admitted by `requires`, within the check’s stated bounds, while running the item’s own body. A *partly checked* seam has a check for only part of its contract; the [seam ledger](SEAMS.md#status) states what remains unchecked. The combined row adds only discharged trusted functions.

The [FLOOR](FLOOR) on main is `functions 82.3`, `lines 92.7`, `combined-functions 84.8`, and `combined-lines 93.6`. The Verus check fails when coverage drops below any figure, or when a figure is set below the base revision's unless a `lowered FIGURE P REASON` line records the decrease. The [README badge](../README.md) shows the `functions` and `lines` figures.

On 2026-10-25, the reference code in tracking PR [#138](https://github.com/wolfi-dev/challoc/pull/138) reported 1274 of 1519 functions (83.8%) and 37648 of 40796 lines (92.2%) verified. Its counter could not parse 1 file, `src/api/mod.rs`, so that comparison excludes part of the reference code. Main verifies 1258 more lines. Its function share is 1.3 points lower, alongside a rise from 221 to 251 trusted functions as more small seams were split out.

## Trust base

The [seam ledger](SEAMS.md) records 408 items in 67 source files that Verus takes on trust. These are items, not coverage lines: `external_body` declarations, axioms, specifications of external code or types, specification implementations, and functions whose termination is assumed.

| Ledger view | Counts |
| --- | --- |
| Status | 129 discharged: 92 by Kani harnesses and 37 by compile-time constant assertions; 34 partly checked; 245 trusted |
| Kind | 152 `external_body`; 12 `external_body` proofs; 7 `external_body` types; 40 `external_body` constants; 86 axioms; 77 assumed specifications; 14 external type specifications; 1 external trait specification; 8 specification implementations; 11 functions without decreases checks |
| Category | 151 core library; 52 allocator invariant; 47 atomics primitive; 31 inline assembly; 21 internal raw pointer; 18 function pointer; 14 panic or abort; 14 C-program contract; 17 platform or kernel fact; 13 FFI or syscall; 13 `UnsafeCell`, volatile, or transmute; 11 termination; 6 raw pointer from a C caller |
| Proposed tool | 196 Kani; 113 none (assumption); 51 loom or GenMC; 31 Islaris or Isla; 13 Verus; 4 Miri (evidence only) |

The 193 trusted functions with no check account for 2194 executable lines. Their ledger categories show where proof work remains:

| Category | Lines | Functions | Why the seam remains |
| --- | ---: | ---: | --- |
| Inline assembly | 582 | 31 | Instruction semantics; Isla is proposed. |
| Atomics primitive | 465 | 39 | Atomic protocol behavior; loom or GenMC is proposed. |
| Allocator invariant | 369 | 49 | Lock invariants and frames, initialization and publication tokens, and facts that fresh memory reads zero. |
| Internal raw pointer | 199 | 11 | Pointer and storage contracts. |
| FFI or syscall | 174 | 11 | Contracts for calls including `pthread_join`, `pthread_setspecific`, `sigprocmask`, `try_reserve`, `spawn`, futex wait and wake, errno access, and `emit`. |
| Platform or kernel fact | 115 | 16 | `mmap` disjointness, fork resume, user virtual-address limits, I/O counts, and PAC sign and authenticate facts. |
| Function pointer | 89 | 8 | Indirect-call contracts. |
| Panic or abort | 62 | 5 | Abort behavior. |
| Raw pointer from a C caller | 61 | 6 | Caller-owned pointer validity. |
| C-program contract | 53 | 14 | `axiom_c_call`, `axiom_c_claim`, `axiom_c_out`, and `axiom_c_writes` assume, among other duties, that callers free only live blocks. |
| `UnsafeCell`, volatile, or transmute | 22 | 2 | Operations Verus does not check directly. |

Among these, the allocator-invariant category includes 14 `axiom_*_lock_inv` and `axiom_*_lock_frame` seams, `axiom_ready`, `axiom_published`, `axiom_state`, `axiom_statics`, `axiom_process_shared`, and fresh-memory facts such as `axiom_fresh_slots_zero`, `axiom_fresh_anon_zero`, and `axiom_discarded_zero`.

The counter also warns about 1 function without a ledger entry: the 3-line `#[verifier::external]` `Default for Stderr` implementation in [sys/abort.rs](../src/sys/abort.rs).

On main, 134 `#[kani::proof]` harnesses under `src/` check bounded seam contracts with Kani `0.68.0` and CBMC `6.11.0`. Each harness comment names its seam and bounds. The source also has 50 top-level `const _: () = assert!(..)` assertions; the compiler evaluates them on both quanta and targets. The 61 loom model tests in `tests/loom` provide evidence for atomic protocols, but no seam status cites loom yet. Isla and emptyasm are not on main.

CI keeps this evidence connected to the ledger. `sh verus/seams.sh --check` fails if the ledger is stale, a seam lacks a classification, a discharged Kani citation names no proof harness, or a constant-assertion citation fails to identify exactly 1 assertion. The presubmit `kani` check runs for changes to `src/kani/`, `src/vspec/`, and listed source files. The postsubmit `kani-ignoring` check runs every harness at quantum 16 and quantum 8, in 4 shards per quantum, and fails if one does not verify or CBMC prints a `warning: ignoring` line.

## Process guarantees

A rewrite made only for Verus must preserve behavior and leave the release machine code of `malloc`, `free`, `calloc`, `realloc`, `sdallocx`, `free_sized`, and everything inlined into them unchanged. [CONTRIBUTING.md](../CONTRIBUTING.md) and the [Verus conventions](README.md#conventions) require `scripts/codegen-diff.sh compare` to report `changed 0` for that hot set; relocation-only differences are acceptable. The `codegen` and `codegen-arm64` CI checks compare base to head on x86_64 and native arm64 at both quanta.

A specification change gets a mutation check: change the code in the way the specification should reject, then confirm that Verus rejects it.

Guard tests constrain where trusted permissions and copied proof bodies can be used:

| Guard | What it checks |
| --- | --- |
| [sys/mod.rs](../src/sys/mod.rs) | `grant_syscalls` appears only in an `extern "C"` body or `abort_fmt`; `grant_aborts` appears only in an `extern "C"` body or the panic handler; `grant_user_reads` appears once each in `purge::check_zero`, `api/libc`’s `realloc_block` and `realloc_usable_stored`, and `api/jemalloc`’s `rallocx_zeroed`. |
| [lock.rs](../src/lock.rs) | `axiom_acquired` appears only in `Lock::lock` and `try_lock`, where they build a guard, and in `Lock::lock_held_perm`; and `axiom_held` only in `Lock::get_unlocked`. |
| [heap/tcaches.rs](../src/heap/tcaches.rs) | `axiom_heap_use` appears once in each of the 8 documented mint sites. |
| [api/restated.rs](../src/api/restated.rs) | The 3 restated-body tests compare names and order of executable calls with their source bodies. They do not compare arguments or control flow. |
| [tests/loom/tests/pins.rs](../tests/loom/tests/pins.rs) | 258 copied source lines stay pinned; every atomic ordering, fence, or futex line in a pinned function is pinned or waived with a reason. |
| [tests/loom/tests/kani_trigger.rs](../tests/loom/tests/kani_trigger.rs) | Every file with a `kani_*` module appears in the Kani presubmit list or `kani-ignoring`. |

The `verus` presubmit check (x86_64) runs the seam check, quantum 16 proofs, counter tests, the coverage floor against the PR base, and quantum 8 proofs; `verus-arm64` runs the same steps, without the counter tests, natively on arm64. `verus-main` runs postsubmit against the preceding main commit. The `lint` checks include rustfmt, clippy, loom, shellcheck, the codegen function-comparison self-test, `kani-boolcmp`, and an MSRV build. `kani-boolcmp` rejects ordering comparisons on Booleans in Kani code because CBMC `6.11` ignores them and could let a harness pass without checking that comparison.

Verus, codegen, and Linux tests run natively on both x86_64 and arm64 in CI. The postsubmit floor checks also run `make test` and `make fuzz` on AlmaLinux 9’s 5.14 kernel on GCE VMs for both architectures.

## Known limits and open work

[Issue #397](https://github.com/wolfi-dev/challoc/issues/397) tracks the remaining proof and audit work.

- **Lock release:** Unlock has not been verified to restore the lock invariant. Draft [#396](https://github.com/wolfi-dev/challoc/pull/396) covers Meta; tcaches, sets, and stats follow, then arena, class, huge, and table. `unlock_held_perm` and `get_unlocked` have the same gap.
- **Instruction semantics:** On main, the 31 inline-assembly seams (582 lines) have no check. The Isla PRs in progress would cite Isla on 28 seams, 7 discharged and 21 partly checked; the parts they leave unchecked include the kernel side of syscalls, PAC fixed data key A and EnDA for sign and authenticate, failure to resume past a trap, and TLS or GOT entry contents.
- **C boundary:** The `errno` of nullable exports ([#394](https://github.com/wolfi-dev/challoc/pull/394)) and the out-pointer results of `posix_memalign` and the allocating `challoc_*_usable` exports reach the C caller through trusted hand-back axioms, `axiom_errno_at_return` and `axiom_out_at_return`. A shape test, not Verus, checks where the exports call them and each export’s `failed` condition.
- **Seam drift:** `seams.sh` does not hash contracts or bodies. An edit could leave a stale discharged or partly checked status. Audit test 8 adds the hashes, after the other seam PRs land.
- **Remaining contracts:** `TlsCell::kept` needs a stronger empty-slot contract, and the R2c mutation that restores 0 remains a stated limit. Draft [#400](https://github.com/wolfi-dev/challoc/pull/400) replaces `axiom_raw_valid` with an `in_alloc`-only axiom.
- **Restated paths:** G17’s no-syscall and read-only theorems restate fast-path bodies instead of calling them, and the 2 `mallctl` theorems restate the bodies of `mallctl` and `mallctlbymib`. The restated-body tests guard call names and order; for the fast-path theorem, the split that would let it call the real bodies is held back by hot-path codegen risk.
- **Deferred audit rows:** G4 covers the creation lock while verified code uses `Arenas::get_frozen`’s `&Created`; G7 records whether cache entries were signed; G9 specifies `api::shared_base` and `shared_ptr`; G10 removes `hdr_mut`; G13 separates a no-abort claim from the expected fault on a corrupt PAC-signed TLS slot. Guard tests remain to be added for other convention-audit rows.
- **Theorem scope:** The stated theorems do not cover `mallctl` and `mallctlbymib` calls for the 4 `thread.*` readers or for operations other than utilization queries, `realloc`’s move path, or `mallinfo2` arithmetic, which wraps in the release build. `theorem_metadata_integrity` does not tie atomic descriptor and owner writes or table bitmap-word permissions to their addresses, cover the last bitmap word when its bit count is not a multiple of 64, cover arenas’ slab-set bitmaps, or exclude TLS in a program-allocated thread stack.

## In progress

These PRs are drafts and are **not merged**:

- [#389](https://github.com/wolfi-dev/challoc/pull/389): Isla I1 checks aarch64 syscalls and `tls::get_tp`; 8 seams are partly checked. Across #389 and the unopened I2, I3 and I4, 28 seams cite Isla: 7 discharged and 21 partly checked. Native arm64 runs passed at quantum 16 and quantum 8.
- [#394](https://github.com/wolfi-dev/challoc/pull/394): State nullable exports’ errno at the C boundary.
- [#396](https://github.com/wolfi-dev/challoc/pull/396): Verify that unlock restores the Meta lock invariant.
- [#400](https://github.com/wolfi-dev/challoc/pull/400): Remove `axiom_raw_valid`.
- [#401](https://github.com/wolfi-dev/challoc/pull/401): Add Kani checks for zero axioms and mallctl’s root; 2 seams discharged and 3 partly checked.
- [#402](https://github.com/wolfi-dev/challoc/pull/402): Verify bitmap-allocation and refill aborts; add 77 verified lines.
- [#413](https://github.com/wolfi-dev/challoc/pull/413): Verify `env_var`, `emit` and `parse_from` at the C boundary (W1c).
- [#391](https://github.com/wolfi-dev/challoc/pull/391): Re-park a heap created during `pthread_join`.

Isla I2 (x86_64), I3 (trap, TLS, GOT, and rseq), I4 (pointer authentication), emptyasm, and Isla CI are not yet opened.

## Summary for slides

- 92.8% of executable lines and 82.5% of executable functions are Verus-verified.
- 93.6% of lines and 84.9% of functions count when fully discharged Kani and constant-assertion seams are included.
- 58 theorems cover metadata integrity, live-block separation, guard pages, zeroing, free paths, thread exit, and accounting.
- `theorem_metadata_integrity` separates returned blocks from the metadata ranges named in its contract, subject to stated trust assumptions and exclusions.
- The covered fast paths make no syscall except an integrity abort’s write and `SIGABRT`; valid frees avoid invalid-free and double-free checks.
- 134 Kani harnesses check bounded contracts; 61 loom tests model atomic protocols.
- The proof-change rule requires unchanged hot-path machine code, with `changed 0` in codegen comparisons.
- Verus, codegen, and tests run on x86_64 and native arm64.

| Measure at `a1165e7f` | Count |
| --- | ---: |
| Verus-verified executable lines / functions | 38906 of 41901 (92.8%) / 1272 of 1541 (82.5%) |
| Verified plus discharged lines / functions | 39252 of 41901 (93.6%) / 1309 of 1541 (84.9%) |
| Theorems | 58: 44 proof, 14 verification-only executable |
| Seams | 408: 129 discharged, 34 partly checked, 245 trusted |
| Kani proof harnesses | 134 |
| Trusted functions with no check | 193 functions, 2194 lines |

## Regenerating these numbers

Run these commands at the revision whose figures you want to report:

```sh
make verus
(cd verus/count && cargo run --release -- --floor ../FLOOR ../..)
(cd verus/count && cargo run --release -- --list ../..)
sh verus/seams.sh
grep -rh 'Status: ' verus/ledger
cargo test --release --locked restated
git grep -nE '^\s*(pub(\([a-z]+\))? )?(proof )?fn theorem_' src
```

`make verus` runs the seam check, proofs, and coverage floor. The counter prints verified and combined coverage; `--list` shows individual functions. `sh verus/seams.sh` prints seam totals by category, proposed tool, and kind, with entries under [ledger/src](ledger/src). `cargo test ... restated` checks the restated-body call lists. The `git grep` lists the `theorem_*` definitions; a line with `proof fn` is a proof function, and the rest are verification-only executable functions.
